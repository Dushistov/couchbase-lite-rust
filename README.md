# couchbase-lite-rust [![Build Status](https://github.com/Dushistov/couchbase-lite-rust/actions/workflows/main.yml/badge.svg?branch=master)](https://github.com/Dushistov/couchbase-lite-rust/actions?query=workflow%3ACI+branch%3Amaster)

Rust wrapper for couchbase-lite-core lightweight, embedded, syncable NoSQL database engine.

Quote from [couchbase-lite-core](https://github.com/couchbase/couchbase-lite-core)

> **Couchbase Lite Core** (aka **LiteCore**) is the next-generation core storage and query engine for [Couchbase Lite][CBL]. It provides a cross-platform implementation of the database CRUD and query features, document versioning, and replication/sync.
> All platform implementations of Couchbase Lite (from 2.0 onward) are built atop this core, adding higher-level language & platform bindings. But LiteCore may find other uses too, perhaps for applications that want a fast minimalist data store with map/reduce indexing and queries, but don't need the higher-level features of Couchbase Lite.

## Optional features

### couchbase-lite-core-sys

Build script can either download couchbase-lite-core library (C/C++) with help of git ("git-download" feature),
or you can provide path to source code via `COUCHBASE_LITE_CORE_SRC_DIR`environment variable.
After that build script can run cmake and proper build command for you ("build" feature)
or you can build couchbase-lite-core by yourself and provide path to build directory via `COUCHBASE_LITE_CORE_BUILD_DIR` environment variable.
Also it is possible that static libraries in `COUCHBASE_LITE_CORE_BUILD_DIR` has unique placement,
for example if you use cmake to generate XCode/Visual Studio project,
then you can use `COUCHBASE_LITE_CORE_BUILD_DIRS` environment variable in such way: "directory/with/library1^directory/with/library2".
