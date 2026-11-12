ARG COUCHBASE_VERSION=6.6.0

FROM docker.io/couchbase/server:enterprise-$COUCHBASE_VERSION

ARG COUCHBASE_VERSION
ENV COUCHBASE_VERSION=$COUCHBASE_VERSION

COPY configure-node.sh /etc/service/config-couchbase/run
RUN chmod +x /etc/service/config-couchbase/run
RUN chown -R couchbase:couchbase /etc/service
