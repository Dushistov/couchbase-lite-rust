FROM docker.io/library/ubuntu:22.04

ARG SG_VERSION=3.0.9
ARG SG_DEB_ARM64
ARG SG_DEB_AMD64
ARG SSL=true
ARG LEGACY_MODE=false

RUN apt -yqq update 
RUN apt -yqq install curl

RUN mkdir -p /opt/sg && \
    ARCHITECTURE="$(dpkg --print-architecture)" && \
    if [ "$ARCHITECTURE" = "amd64" ]; then \
        SG_PACKAGE_ARCH="x86_64"; \
        SG_DEB_URL="$SG_DEB_AMD64"; \
    elif [ "$ARCHITECTURE" = "arm64" ]; then \
        SG_PACKAGE_ARCH="aarch64"; \
        SG_DEB_URL="$SG_DEB_ARM64"; \
    else \
        echo "Unsupported architecture"; \
        exit 1; \
    fi && \
    if [ -z "$SG_DEB_URL" ]; then \
        SG_DEB_URL="https://packages.couchbase.com/releases/couchbase-sync-gateway/$SG_VERSION/couchbase-sync-gateway-community_${SG_VERSION}_${SG_PACKAGE_ARCH}.deb"; \
    fi && \
    curl --fail --location --output /opt/sg/couchbase-sync-gateway.deb "$SG_DEB_URL"

COPY cert /opt/sg/cert
COPY config /opt/sg/config
COPY scripts /opt/sg/scripts

WORKDIR /opt/sg

RUN apt -yqq install systemctl
RUN dpkg -i ./couchbase-sync-gateway.deb

EXPOSE 4984
EXPOSE 4985
EXPOSE 4884
EXPOSE 4885

ENV SSL=${SSL}
ENV LEGACY_MODE=${LEGACY_MODE}

CMD ./scripts/start.sh ${SSL} ${LEGACY_MODE}
