# Sync Gateway test fixture

Run `./ci/docker-up-down.py up` and `./ci/docker-up-down.py down` from the repository root. The default uses Docker with sudo. For rootless Podman, add `--engine podman --no-sudo` to both commands; Podman also needs a Compose provider such as `podman-compose`.

The default fixture uses Couchbase Server Enterprise 6.6.0 and Sync Gateway Community 3.0.9. It starts the server and the legacy Sync Gateway database `scratch-30` on public port 4884 and admin port 4885. Server 6.6 and Sync Gateway 3.0 do not support named scopes and collections, so the collections gateway is opt in.

`./ci/build_and_run_tests.py --with-server-only` runs the ignored replication tests against `scratch-30`. Add `--with-valigrind` to run them under Valgrind. The ordering test uses 10,000 documents by default; set `SG_TEST_DOCUMENT_COUNT=100` for a quick local check.

The `chat-stress` job uses `--stress-only`, which starts the same two services without creating the test user. Its driver provisions its own user through the admin API. Ordinary `up` creates `sguser/password` for the replication tests.

To run the newer pair used by the scheduled `network-tests` matrix, set `COUCHBASE_VERSION=8.0.3` and `SG_VERSION=4.1.2` before `up`. The two variables select the Couchbase Server image and Sync Gateway Community package. The Compose file also accepts `SG_LEGACY_DEB_AMD64` and `SG_LEGACY_DEB_ARM64` to override the legacy package URL.

For manual collection testing with Server 7+ and Sync Gateway 3.1+, add `--with-collections` to `up` and `down`. This starts a second Sync Gateway database, `scratch`, with collections `flowers.roses`, `flowers.tulips`, and `flowers.lavenders` on public port 4984 and admin port 4985. `SG_DEB_AMD64` and `SG_DEB_ARM64` can override that gateway's package URL. The default minimum pair cannot run this service.

The Rust native build applies `couchbase-lite-core-sys/patches/fleece-shared-buffer.patch` to the Core source checkout before compiling it. This fixes a zero-length Fleece slice allocation that Valgrind detects as an invalid write. `ci/valgrind-suppressions.supp` limits suppression of observed HTTP header warnings to WebSocket handshake paths.
