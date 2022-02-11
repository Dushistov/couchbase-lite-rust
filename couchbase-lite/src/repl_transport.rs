use crate::{
    ffi::{
        c4error_make, c4socket_closeRequested, c4socket_closed, c4socket_completedWrite,
        c4socket_gotHTTPResponse, c4socket_opened, c4socket_received, c4socket_registerFactory,
        kC4NetErrInvalidURL, kC4NetErrTLSHandshakeFailed, kC4NoFraming, kC4NumNetErrorCodesPlus1, kC4ReplicatorOptionCookies,
        kC4ReplicatorOptionExtraHeaders, kC4SocketOptionWSProtocols,
        kWebSocketCloseBadMessageFormat, kWebSocketCloseFirstAvailable, kWebSocketCloseNormal,
        C4Address, C4Error, C4Slice, C4SliceResult, C4Socket, C4SocketFactory, C4SocketFraming,
        C4String, FLDict_Get, FLEncoder_BeginDict, FLEncoder_EndDict, FLEncoder_Finish,
        FLEncoder_Free, FLEncoder_New, FLEncoder_WriteKey, FLEncoder_WriteString,
        FLError_kFLNoError, FLTrust_kFLUntrusted, FLValue_AsDict, FLValue_FromData, FleeceDomain,
        NetworkDomain, WebSocketDomain,
    },
    fl_slice::{fl_slice_to_slice, fl_slice_to_str_unchecked, AsFlSlice, FlSliceOwner},
    value::ValueRef,
};
use futures_util::{sink::SinkExt, stream::StreamExt};
use http::{HeaderValue, Uri};
use log::{debug, error, info, trace, warn};
use std::{
    borrow::Cow,
    convert::TryFrom,
    fmt, mem,
    os::raw::{c_int, c_void},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    }, thread
};
use tokio::{
    net::TcpStream,
    runtime::Handle,
    sync::Mutex as TokioMutex,
    sync::{oneshot, Notify},
};
use tokio_tungstenite::{connect_async, tungstenite::{
    self,
    handshake::client::{Request, Response},
    protocol::{frame::coding::CloseCode, CloseFrame, Message},
}};
use std::time::{Duration, Instant};

/// use embedded web-socket library
pub fn use_web_sockets(handle: Handle) {
    let handle = Arc::new(handle);
    let sock_factory = C4SocketFactory {
        context: Arc::into_raw(handle) as *mut c_void,
        framing: kC4NoFraming as C4SocketFraming,

        open: Some(ws_open),
        write: Some(ws_write),
        completedReceive: Some(ws_completed_receive),
        close: None,
        requestClose: Some(ws_request_close),
        dispose: Some(ws_dispose),
    };

    unsafe { c4socket_registerFactory(sock_factory) };
}

type WsWriter = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    Message,
>;

struct Socket {
    handle: Arc<Handle>,
    writer: Arc<TokioMutex<Option<WsWriter>>>,
    stop_read: Arc<TokioMutex<Option<oneshot::Sender<()>>>>,
    read_data_avaible: AtomicUsize,
    read_confirmed: Arc<Notify>,
    close_confirmied: Arc<Notify>,
    closed: Arc<TokioMutex<bool>>,
    c4sock: usize,
    last_activity: Arc<TokioMutex<Instant>>,
}

impl Socket {
    fn close(self: Arc<Self>) {
        let c4sock = self.c4sock;
        trace!("Socket::close, c4sock {:x}", c4sock);
        let writer = self.writer.clone();
        let stop_read = self.stop_read.clone();
        self.handle.spawn(async move {
            {
                let mut writer = writer.lock().await;
                *writer = None;
            }
            let stop_read: Option<oneshot::Sender<()>> = {
                let mut stop_read = stop_read.lock().await;
                stop_read.take()
            };
            if let Some(stop_read) = stop_read {
                let _ = stop_read.send(());
            }
            trace!("Socket::close, c4sock {:x} DONE", c4sock);
        });
    }
}

unsafe extern "C" fn ws_open(
    c4sock: *mut C4Socket,
    addr: *const C4Address,
    options: C4Slice,
    context: *mut c_void,
) {
    assert!(!context.is_null());
    let handle: Arc<Handle> = Arc::from_raw(context as *const Handle);

    assert!(!c4sock.is_null());
    let c4sock: &mut C4Socket = &mut *c4sock;
    assert!(c4sock.nativeHandle.is_null());

    assert!(!addr.is_null());
    let addr: &C4Address = &*addr;
    let request = c4address_to_request(c4sock as *mut C4Socket as usize, addr, options);

    let client = Arc::new(Socket {
        handle: handle.clone(),
        writer: Arc::new(TokioMutex::new(None)),
        stop_read: Arc::new(TokioMutex::new(None)),
        read_confirmed: Arc::new(Notify::new()),
        c4sock: c4sock as *mut C4Socket as usize,
        read_data_avaible: AtomicUsize::new(0),
        close_confirmied: Arc::new(Notify::new()),
        last_activity: Arc::new(TokioMutex::new(Instant::now())),
        closed: Arc::new(TokioMutex::new(false))
    });
    trace!(
        "ws_open, c4sock {:x}, uri: {:?}",
        client.c4sock,
        request.as_ref().map(Request::uri)
    );
    let client2 = client.clone();
    let client_for_ping = client.clone();
    c4sock.nativeHandle = Arc::into_raw(client) as *mut c_void;

    handle.spawn(async move {
        open_connection(request, client2).await;
    });

    let handle_for_ping = handle.clone();

    thread::spawn(move || {
        handle_for_ping.block_on(async move {
            loop {
                thread::sleep(Duration::from_secs(5));
                let mut last_activity = client_for_ping.last_activity.lock().await;
                if last_activity.elapsed().as_secs() > 55 {
                    let writer = client_for_ping.writer.clone();
                    let mut writer = writer.lock().await;

                    if let Some(writer) = writer.as_mut() {
                        if writer.send(Message::Ping(vec![])).await.is_err() {
                            error!("writer.send error");
                            break;
                        }
                    } else {
                        error!("Break the loop of ping/hearbeat");
                        break;
                    }
                    *last_activity = Instant::now();
                }
            }
        });
    });

    mem::forget(handle);
}

unsafe extern "C" fn ws_write(c4sock: *mut C4Socket, allocated_data: C4SliceResult) {
    trace!(
        "ws_write: c4sock {:?}, bytes {}",
        c4sock,
        allocated_data.size
    );
    let data: FlSliceOwner = allocated_data.into();
    let data: Vec<u8> = data.as_bytes().to_vec();

    assert!(!c4sock.is_null());
    let c4sock: &mut C4Socket = &mut *c4sock;
    assert!(!c4sock.nativeHandle.is_null());

    let socket: &Socket = &*(c4sock.nativeHandle as *const Socket);
    assert_eq!(c4sock as *mut _ as usize, socket.c4sock);
    let c4sock = socket.c4sock;

    let writer = socket.writer.clone();
    socket.handle.spawn(async move {
        let mut last_activity = socket.last_activity.lock().await;
        *last_activity = Instant::now();

        let mut writer = writer.lock().await;

        if let Some(writer) = writer.as_mut() {
            let n = data.len();
            if let Err(err) = writer.send(Message::Binary(data)).await {
                error!("ws_write: sock {:?}, writer.send failure: {}", c4sock, err);
            } else {
                c4socket_completedWrite(c4sock as *mut _, n);
            }
        }
    });
}

unsafe extern "C" fn ws_completed_receive(c4sock: *mut C4Socket, byte_count: usize) {
    trace!(
        "ws_completed_receive, c4sock {:?}, bytes {}",
        c4sock,
        byte_count
    );
    assert!(!c4sock.is_null());
    let c4sock: &mut C4Socket = &mut *c4sock;
    assert!(!c4sock.nativeHandle.is_null());
    let socket: &Socket = &*(c4sock.nativeHandle as *const Socket);
    assert_eq!(c4sock as *mut _ as usize, socket.c4sock);

    let nbytes = socket.read_data_avaible.load(Ordering::Acquire);
    let nbytes = if nbytes >= byte_count {
        nbytes - byte_count
    } else {
        0
    };
    socket.read_data_avaible.store(nbytes, Ordering::Release);
    if nbytes == 0 {
        let read_confirmed = socket.read_confirmed.clone();
        socket.handle.spawn(async move {
            read_confirmed.notify_one();
        });
    }
}

unsafe extern "C" fn ws_request_close(c4sock: *mut C4Socket, status: c_int, message: C4String) {
    trace!("ws_request_close, c4sock {:?}, status {}", c4sock, status);
    assert!(!c4sock.is_null());
    let c4sock: &mut C4Socket = &mut *c4sock;
    assert!(!c4sock.nativeHandle.is_null());

    let socket: &Socket = &*(c4sock.nativeHandle as *const Socket);
    assert_eq!(c4sock as *mut _ as usize, socket.c4sock);
    let c4sock = socket.c4sock;

    let socket_for_closed = socket.clone();

    let writer = socket.writer.clone();
    let code: CloseCode = u16::try_from(status).unwrap_or(1).into();
    let reason: Cow<'static, str> = Cow::Owned(String::from(fl_slice_to_str_unchecked(message)));
    let close_confirmied = socket.close_confirmied.clone();
    socket.handle.spawn(async move {
        let mut closed = socket_for_closed.closed.lock().await;
        *closed = true;
        let err = c4error_make(WebSocketDomain, status, reason.as_ref().as_flslice());
        let mut writer = writer.lock().await;

        if let Some(writer) = writer.as_mut() {
            if let Err(err) = writer
                .send(Message::Close(Some(CloseFrame { code, reason })))
                .await
            {
                error!(
                    "ws_request_close, c4sock {:x} writer.send failure: {}",
                    c4sock, err
                );
            }
        }

        close_confirmied.notify_one();
        c4socket_closed(c4sock as *mut _, err);
    });
}

unsafe extern "C" fn ws_dispose(c4sock: *mut C4Socket) {
    trace!("ws_dispose, c4sock {:?}", c4sock);

    assert!(!c4sock.is_null());
    let c4sock: &mut C4Socket = &mut *c4sock;
    assert!(!c4sock.nativeHandle.is_null());
    let client: Arc<Socket> = Arc::from_raw(c4sock.nativeHandle as *const _);
    assert_eq!(c4sock as *mut _ as usize, client.c4sock);
    client.close();
}

#[derive(Debug)]
enum InvalidRequest {
    HttpError(http::Error),
    InvalidHeaderValue(http::header::InvalidHeaderValue),
}

impl From<http::Error> for InvalidRequest {
    fn from(err: http::Error) -> Self {
        Self::HttpError(err)
    }
}

impl From<http::header::InvalidHeaderValue> for InvalidRequest {
    fn from(err: http::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeaderValue(err)
    }
}

impl fmt::Display for InvalidRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidRequest::HttpError(err) => write!(f, "{}", err),
            InvalidRequest::InvalidHeaderValue(err) => write!(f, "{}", err),
        }
    }
}

unsafe fn c4address_to_request(
    marker: usize,
    addr: &C4Address,
    options: C4Slice,
) -> Result<Request, InvalidRequest> {
    let uri = Uri::builder()
        .scheme(fl_slice_to_slice(addr.scheme))
        .authority(fl_slice_to_slice(addr.hostname))
        .port(addr.port)
        .path_and_query(fl_slice_to_slice(addr.path))
        .build()?;
    trace!("c4address_to_request, marker {:x}, uri {:?}", marker, uri);
    let mut request = Request::get(uri).body(())?;

    let options = FLValue_AsDict(FLValue_FromData(options, FLTrust_kFLUntrusted));
    if let ValueRef::Dict(_opts) = ValueRef::from(FLDict_Get(
        options,
        slice_without_nul!(kC4ReplicatorOptionExtraHeaders).as_flslice(),
    )) {
        todo!();
    }

    if let ValueRef::String(cookies) = ValueRef::from(FLDict_Get(
        options,
        slice_without_nul!(kC4ReplicatorOptionCookies).as_flslice(),
    )) {
        request
            .headers_mut()
            .insert("Cookie", HeaderValue::from_str(cookies)?);
    }

    if let ValueRef::String(protocol) = ValueRef::from(FLDict_Get(
        options,
        slice_without_nul!(kC4SocketOptionWSProtocols).as_flslice(),
    )) {
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_str(protocol)?);
    }

    Ok(request)
}

unsafe fn headers_to_dict(http_resp: &Response) -> Result<FlSliceOwner, u32> {
    let enc = FLEncoder_New();
    FLEncoder_BeginDict(enc, http_resp.headers().len());
    for (key, value) in http_resp.headers().iter() {
        FLEncoder_WriteKey(enc, key.as_str().as_bytes().as_flslice());
        FLEncoder_WriteString(enc, value.as_bytes().as_flslice());
    }
    FLEncoder_EndDict(enc);
    let mut fl_err = FLError_kFLNoError;
    let res = FLEncoder_Finish(enc, &mut fl_err);
    FLEncoder_Free(enc);
    if fl_err == FLError_kFLNoError {
        Ok(res.into())
    } else {
        Err(fl_err)
    }
}

async fn open_connection(request: Result<Request, InvalidRequest>, socket: Arc<Socket>) {
    let sock_id = socket.c4sock;

    let request = match request {
        Ok(x) => x,
        Err(err) => {
            let msg = err.to_string();
            error!("ws_open({:x}): Can not parse URI: {}", sock_id, msg);
            unsafe {
                let c4err = c4error_make(
                    NetworkDomain,
                    kC4NetErrInvalidURL as c_int,
                    msg.as_bytes().as_flslice(),
                );
                c4socket_closed(sock_id as *mut _, c4err);
            }
            return;
        }
    };
    match connect_async(request).await {
        Ok((ws_stream, http_resp)) => {
            trace!("open_connection c4sock {:x}: websocket openned", sock_id);
            if !*socket.closed.lock().await {
                unsafe {
                    let headers = match headers_to_dict(&http_resp) {
                        Ok(x) => x,
                        Err(fl_err) => {
                            error!(
                                "open_connection: c4sock {:x}, flencoder error: {}",
                                sock_id, fl_err
                            );
                            let c4err =
                                c4error_make(FleeceDomain, fl_err as c_int, "".as_bytes().as_flslice());
                            c4socket_closed(sock_id as *mut _, c4err);
                            return;
                        }
                    };
                    c4socket_gotHTTPResponse(
                        sock_id as *mut _,
                        http_resp.status().as_u16() as c_int,
                        headers.as_flslice(),
                    );
                }

                let (ws_writer, mut ws_reader) = ws_stream.split();

                {
                    let mut lock = socket.writer.lock().await;
                    *lock = Some(ws_writer);
                }
                let (stop_read, mut time_to_stop) = oneshot::channel();
                {
                    let mut lock = socket.stop_read.lock().await;
                    *lock = Some(stop_read);
                }
                unsafe {
                    c4socket_opened(sock_id as *mut C4Socket);
                }

                let read_confirmed = socket.read_confirmed.clone();
                let close_confirmied = socket.close_confirmied.clone();

                'read_loop: loop {
                    tokio::select! {
                    message = ws_reader.next() => {
                        let message = match message {
                            Some(x) => x,
                            None => break 'read_loop,
                        };
                        match message {
                            Ok(m @ Message::Text(_)) | Ok(m @ Message::Binary(_)) => {
                                let data = m.into_data();
                                socket.read_data_avaible.store(data.len(), Ordering::Release);
                                unsafe {
                                    c4socket_received(sock_id as *mut _, data.as_slice().as_flslice());
                                }
                                read_confirmed.notified().await;
                            }
                            Ok(Message::Close(close_frame)) => {
                                info!("read loop({:x}): close", sock_id);
                                let (code, reason) = close_frame.map(|x| (u16::from(&x.code) as c_int, x.reason)).unwrap_or_else(|| {
                                    (-1, "".into())
                                });
                                unsafe {
                                    c4socket_closeRequested(sock_id as *mut C4Socket, code, reason.as_bytes().as_flslice());
                                }
                                close_confirmied.notified().await;
                                break 'read_loop;
                            }
                            Ok(Message::Ping(_)) => {
                                debug!("read loop({:x}): ping", sock_id);
                                todo!();
                            }
                            Ok(Message::Pong(_)) => {
                                //debug!("read loop({:x}): pong", sock_id);
                                //todo!();
                            }
                            Err(err) => {
                                error!("read loop({:x}) message error: {}", sock_id, err);
                                unsafe {
                                    let c4err = tungstenite_err_to_c4_err(err);
                                    c4socket_closed(sock_id as *mut C4Socket, c4err);
                                }
                                break 'read_loop;
                            }
                        }

                    }

                    _ = &mut time_to_stop => {
                        info!("read loop, c4sock {:x}: time to stop signal", sock_id);
                        break 'read_loop;
                    }
                    else => break 'read_loop,

                }
                    ;
                }
            } else {
                warn!("Connection is closed so we cannot open it!");
            }
        }
        Err(err) => unsafe {
            error!(
                "open_connection, c4sock {:x}: connection failed: {}",
                sock_id, err
            );
            let c4err = tungstenite_err_to_c4_err(err);
            c4socket_closed(sock_id as *mut C4Socket, c4err);
        },
    }
}

unsafe fn tungstenite_err_to_c4_err(err: tungstenite::Error) -> C4Error {
    use tungstenite::error::Error::*;
    let msg = err.to_string();
    let (domain, code) = match err {
        ConnectionClosed => (WebSocketDomain, kWebSocketCloseNormal),
        AlreadyClosed => (WebSocketDomain, kWebSocketCloseFirstAvailable),
        Io(_) => (NetworkDomain, kC4NumNetErrorCodesPlus1),
        #[cfg(feature = "tls")]
        Tls(_) => (NetworkDomain, kC4NumNetErrorCodesPlus1),
        Capacity(_) => (NetworkDomain, kC4NumNetErrorCodesPlus1),
        Protocol(_) => (NetworkDomain, kC4NumNetErrorCodesPlus1),
        SendQueueFull(_) => (NetworkDomain, kC4NumNetErrorCodesPlus1),
        Utf8 => (WebSocketDomain, kWebSocketCloseBadMessageFormat),
        Url(_) => (NetworkDomain, kC4NetErrInvalidURL),
        Http(ref code) => (WebSocketDomain, u32::from(code.status().as_u16())),
        HttpFormat(_) => (WebSocketDomain, kWebSocketCloseBadMessageFormat),
        Tls(_) => (NetworkDomain, kC4NetErrTLSHandshakeFailed),
    };
    c4error_make(domain, code as c_int, msg.as_bytes().as_flslice())
}
