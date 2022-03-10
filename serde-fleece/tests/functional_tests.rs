use ffi::{
    FLEncoder_Free, FLEncoder_New, FLSliceResult, FLTrust, FLValue_FromData, FLValue_ToJSON,
    _FLEncoder,
};
use rustc_hash::FxHashMap;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_fleece::*;
use std::ptr::NonNull;

#[derive(Serialize, Debug, PartialEq, Deserialize, Hash, Eq)]
struct Millimeters(u8);

#[test]
fn test_ser_primitive() {
    assert_eq!("true", to_fleece_to_json(&true));
    assert_eq!("false", to_fleece_to_json(&false));
    assert_eq!("-9223372036854775808", to_fleece_to_json(&i64::min_value()));
    assert_eq!("9223372036854775807", to_fleece_to_json(&i64::max_value()));
    assert_eq!("0", to_fleece_to_json(&0_i64));
    assert_eq!(
        "\"This is text, привет\"",
        to_fleece_to_json(&"This is text, привет")
    );
    assert_eq!(
        r#"[false,17,"Как","Ч"]"#,
        to_fleece_to_json(&(false, 17, "Как", 'Ч'))
    );
    assert_eq!("null", to_fleece_to_json(&Option::<i32>::None));
    assert_eq!("17", to_fleece_to_json(&Some(17)));
    assert_eq!("null", to_fleece_to_json(&()));
    assert_eq!("\"ж\"", to_fleece_to_json(&'ж'));
}

#[test]
fn test_ser_primitive_with_shared_encoder() {
    let mut enc = Encoder::new();
    assert_eq!("true", to_fleece_to_json_enc(&true, enc.session()));
    assert_eq!("false", to_fleece_to_json_enc(&false, enc.session()));
    assert_eq!(
        "-9223372036854775808",
        to_fleece_to_json_enc(&i64::min_value(), enc.session())
    );
    assert_eq!(
        "9223372036854775807",
        to_fleece_to_json_enc(&i64::max_value(), enc.session())
    );
    assert_eq!("0", to_fleece_to_json_enc(&0_i64, enc.session()));
    assert_eq!(
        "\"This is text, привет\"",
        to_fleece_to_json_enc(&"This is text, привет", enc.session())
    );
    assert_eq!(
        r#"[false,17,"Как","Ч"]"#,
        to_fleece_to_json_enc(&(false, 17, "Как", 'Ч'), enc.session())
    );
    assert_eq!(
        "null",
        to_fleece_to_json_enc(&Option::<i32>::None, enc.session())
    );
    assert_eq!("17", to_fleece_to_json_enc(&Some(17), enc.session()));
    assert_eq!("null", to_fleece_to_json_enc(&(), enc.session()));
    assert_eq!("\"ж\"", to_fleece_to_json_enc(&'ж', enc.session()));
}

#[test]
fn test_ser_struct() {
    #[derive(Serialize)]
    struct Test {
        int: u32,
        seq: Vec<&'static str>,
    }

    let test = Test {
        int: 1,
        seq: vec!["a", "b"],
    };
    assert_eq!(
        "{\"int\":1,\"seq\":[\"a\",\"b\"]}",
        to_fleece_to_json(&test)
    );
    #[derive(Serialize)]
    struct Test2 {
        int: u32,
        seq: Vec<String>,
        opt: Option<f32>,
    }

    let test = Test2 {
        int: 1,
        seq: vec!["a".into(), "b".into()],
        opt: Some(0.0),
    };
    assert_eq!(
        r#"{"int":1,"opt":0.0,"seq":["a","b"]}"#,
        to_fleece_to_json(&test)
    );

    #[derive(Serialize)]
    struct Unit;
    assert_eq!("null", to_fleece_to_json(&Unit));

    assert_eq!("15", to_fleece_to_json(&Millimeters(15)));

    #[derive(Serialize)]
    struct Rect(u16, u16);
    assert_eq!("[1,400]", to_fleece_to_json(&Rect(1, 400)));
}

#[test]
fn test_ser_enum() {
    #[derive(Serialize)]
    enum E {
        Unit,
        Newtype(u32),
        Tuple(u32, u32),
        Struct { a: u32, b: i8 },
    }

    assert_eq!(r#""Unit""#, to_fleece_to_json(&E::Unit));
    assert_eq!(r#"{"Newtype":1}"#, to_fleece_to_json(&E::Newtype(1)));
    assert_eq!(r#"{"Tuple":[1,2]}"#, to_fleece_to_json(&E::Tuple(1, 2)));
    assert_eq!(
        r#"{"Struct":{"a":1,"b":127}}"#,
        to_fleece_to_json(&E::Struct { a: 1, b: 127 })
    );
}

#[test]
fn test_ser_collections() {
    assert_eq!("[1,2]", to_fleece_to_json(&[1_u8, 2_u8]));

    assert_eq!("[1,2,3]", to_fleece_to_json(&vec![1, 2, 3]));
    assert_eq!("[]", to_fleece_to_json(&Vec::<i32>::new()));
    let mut m = FxHashMap::<&str, i32>::default();
    m.insert("15", 15);
    m.insert("17", 17);
    assert_eq!(r#"{"15":15,"17":17}"#, to_fleece_to_json(&m));
    let mut m = FxHashMap::<i32, i32>::default();
    m.insert(5, 10);
    m.insert(6, 11);
    assert_eq!(r#"{"5":10,"6":11}"#, to_fleece_to_json(&m));

    assert_eq!("{}", to_fleece_to_json(&FxHashMap::<i32, i32>::default()));

    let mut m = FxHashMap::<Millimeters, i32>::default();
    m.insert(Millimeters(5), 35);
    m.insert(Millimeters(6), 42);
    assert_eq!(r#"{"5":35,"6":42}"#, to_fleece_to_json(&m));
}

macro_rules! test_primive_ser_deser {
    ($($ty:ty)*) => {
        $(
            let expect = <$ty>::min_value();
            assert_eq!(expect, ser_deser(&expect).unwrap());
            let expect = i64::max_value();
            assert_eq!(expect, ser_deser(&expect).unwrap());
            let expect = 0 as $ty;
            assert_eq!(expect, ser_deser(&expect).unwrap());
        )*
    };
}

#[test]
fn test_de_primitive() {
    assert_eq!(true, ser_deser(&true).unwrap());
    assert_eq!(false, ser_deser(&false).unwrap());
    test_primive_ser_deser!(i8 i16 i32 i64 u8 u16 u32 u64);
    assert_eq!(-1_i32, ser_deser(&-1_i32).unwrap());
    assert_eq!(-1e10f32, ser_deser(&-1e10f32).unwrap());
    assert_eq!(-1e10f64, ser_deser(&-1e10f64).unwrap());
    assert_eq!("Ну что?", ser_deser(&"Ну что?".to_string()).unwrap());
    let expect = 'ю';
    assert_eq!(expect, ser_deser(&expect).unwrap());
}

#[test]
fn test_de_struct_normal_functionality() {
    #[derive(Serialize, PartialEq, Deserialize, Debug)]
    struct Test {
        int: u32,
        seq: Vec<String>,
        opt: Option<f32>,
    }

    let test = Test {
        int: 1,
        seq: vec!["a".into(), "b".into()],
        opt: Some(0.0),
    };
    assert_eq!(test, ser_deser(&test).unwrap());
    let test = Test {
        int: 500,
        seq: vec!["a".into(), "b".into()],
        opt: None,
    };
    assert_eq!(test, ser_deser(&test).unwrap());
    let test = Test {
        int: 44,
        seq: vec![],
        opt: Some(1.0),
    };
    assert_eq!(test, ser_deser(&test).unwrap());
    #[derive(Serialize, Debug, PartialEq, Deserialize)]
    struct Unit;
    assert_eq!(Unit, ser_deser(&Unit).unwrap());

    assert_eq!(Millimeters(15), ser_deser(&Millimeters(15)).unwrap());
    #[derive(Serialize, Debug, PartialEq, Deserialize)]
    struct Rect(u16, u16);
    assert_eq!(Rect(1, 400), ser_deser(&Rect(1, 400)).unwrap());

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    #[serde(tag = "type")]
    struct Foo {
        i: i32,
        s: String,
    }
    let expect = Foo {
        i: 622521,
        s: "Так себе".into(),
    };
    assert_eq!(expect, ser_deser(&expect).unwrap());

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct SkipField {
        i: i32,
        #[serde(default)]
        #[serde(skip_deserializing)]
        s: String,
    }
    assert_eq!(
        SkipField {
            i: 17,
            s: String::new(),
        },
        ser_deser(&SkipField {
            i: 17,
            s: "aaaa".into(),
        })
        .unwrap()
    );
}

#[test]
fn test_de_struct_errors_corner_cases() {
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct S1 {}
    assert_eq!(S1 {}, ser_deser(&S1 {}).unwrap());
    assert_eq!(
        S1 {},
        from_slice::<S1>(fleece!({ "f1": 17 }).unwrap().as_bytes()).unwrap()
    );

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct S2 {
        f1: i32,
    }
    assert_eq!(
        "Custom error: missing field `f1`",
        from_slice::<S2>(fleece!({}).unwrap().as_bytes())
            .unwrap_err()
            .to_string()
    );
    assert_eq!(
        "Custom error: missing field `f1`",
        from_slice::<S2>(fleece!({"f2": 17}).unwrap().as_bytes())
            .unwrap_err()
            .to_string()
    );
    assert_eq!(
        "Custom error: missing field `f1`",
        from_slice::<S2>(fleece!({"f2": 17, "f3": "aaa"}).unwrap().as_bytes())
            .unwrap_err()
            .to_string()
    );

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct S3 {
        i: i32,
        #[serde(default)]
        #[serde(skip_serializing)]
        s: String,
    }
    assert_eq!(
        S3 {
            i: 17,
            s: String::new(),
        },
        from_slice::<S3>(fleece!({"i": 17}).unwrap().as_bytes()).unwrap()
    );

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct S4 {
        i: i32,
        s: String,
    }
    assert_eq!(
        S4 {
            i: 34,
            s: "uuu".into(),
        },
        from_slice::<S4>(fleece!({"s": "uuu", "i": 34}).unwrap().as_bytes()).unwrap()
    );
}

#[test]
fn test_de_enum() {
    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    enum E {
        Unit,
        Newtype(u32),
        Tuple(u32, u32),
        Struct { a: u32 },
    }

    let expected = E::Unit;
    assert_eq!(expected, ser_deser(&expected).unwrap());

    let expected = E::Newtype(1);
    assert_eq!(expected, ser_deser(&expected).unwrap());

    let expected = E::Tuple(1, 2);
    assert_eq!(expected, ser_deser(&expected).unwrap());

    let expected = E::Struct { a: 1 };
    assert_eq!(expected, ser_deser(&expected).unwrap());
}

#[test]
fn test_de_collections() {
    let expect = [1_u8, 2_u8];
    assert_eq!(expect, ser_deser(&expect).unwrap());
    let expect = [1_i32, 2, 3];
    assert_eq!(
        &expect,
        ser_deser(&vec![1_i32, 2_i32, 3_i32]).unwrap().as_slice()
    );
    let expect = Vec::<i32>::new();
    assert_eq!(expect, ser_deser(&expect).unwrap());

    let mut m = FxHashMap::<String, i32>::default();
    m.insert("15".into(), 15);
    m.insert("17".into(), 17);
    assert_eq!(m, ser_deser(&m).unwrap());

    let mut m = FxHashMap::default();
    m.insert(5, 10);
    m.insert(6, 11);
    assert_eq!(m, ser_deser(&m).unwrap());

    let m = FxHashMap::<i32, i32>::default();
    assert_eq!(m, ser_deser(&m).unwrap());

    let mut m = FxHashMap::<Millimeters, i32>::default();
    m.insert(Millimeters(5), 35);
    m.insert(Millimeters(6), 42);
    assert_eq!(m, ser_deser(&m).unwrap());
}

#[test]
fn test_de_fleece_dict() {
    #[derive(Deserialize, PartialEq, Debug)]
    struct JsonData {
        a: i32,
        b: String,
    }
    let mut md = MutableDict::new().unwrap();
    md.set_i64("a", 5);
    md.set_string("b", "16");
    let data: JsonData = from_fl_dict(md.as_dict()).unwrap();
    assert_eq!(
        JsonData {
            a: 5,
            b: "16".into()
        },
        data
    );
}

#[test]
fn test_fleece_macro() {
    assert_eq!(
        r#"{"pattern":"%555"}"#,
        fleece_encoded_to_json(fleece!({ "pattern": "%555" }).unwrap())
    );
    let cookie_name = "cookies";
    let token = "AAAA=BBBB";
    assert_eq!(
        r#"{"cookies":"AAAA=BBBB"}"#,
        fleece_encoded_to_json(fleece!({ cookie_name: token }).unwrap())
    );
    assert_eq!(
        r#"{"f1":17}"#,
        fleece_encoded_to_json(fleece!({ "f1": 17 }).unwrap())
    );
}

fn to_fleece_to_json<T: Serialize>(value: &T) -> String {
    let data = to_fl_slice_result(value).unwrap();
    let val = unsafe { FLValue_FromData(data.as_fl_slice(), FLTrust::kFLUntrusted) };
    assert!(!val.is_null());
    let json = unsafe { FLValue_ToJSON(val) };
    let json: &str = json.as_fl_slice().try_into().unwrap();
    json.to_string()
}

struct Encoder {
    inner: NonNull<_FLEncoder>,
}

impl Encoder {
    fn new() -> Self {
        let enc = unsafe { FLEncoder_New() };
        let enc = NonNull::new(enc).unwrap();
        Encoder { inner: enc }
    }

    fn session(&mut self) -> FlEncoderSession {
        FlEncoderSession::new(self.inner)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { FLEncoder_Free(self.inner.as_ptr()) };
    }
}

fn to_fleece_to_json_enc<T: Serialize>(value: &T, enc: FlEncoderSession) -> String {
    let data = to_fl_slice_result_with_encoder(value, enc).unwrap();
    let val = unsafe { FLValue_FromData(data.as_fl_slice(), FLTrust::kFLUntrusted) };
    assert!(!val.is_null());
    let json = unsafe { FLValue_ToJSON(val) };
    let json: &str = json.as_fl_slice().try_into().unwrap();
    json.to_string()
}

fn ser_deser<T: Serialize + DeserializeOwned>(value: &T) -> Result<T, Error> {
    let ba = to_fl_slice_result(&value)?;
    from_slice(ba.as_bytes())
}

fn fleece_encoded_to_json(data: FLSliceResult) -> String {
    let val = unsafe { FLValue_FromData(data.as_fl_slice(), FLTrust::kFLUntrusted) };
    assert!(!val.is_null());
    let json = unsafe { FLValue_ToJSON(val) };
    let s: &str = json.as_fl_slice().try_into().unwrap();
    s.to_string()
}
