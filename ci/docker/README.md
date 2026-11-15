# Sync Gateway test fixture

Run `./ci/docker-up-down.py up` and `./ci/docker-up-down.py down` from the repository root. The default uses Docker with sudo. For rootless Podman, add `--engine podman --no-sudo` to both commands; Podman also needs a Compose provider such as `podman-compose`.

The default fixture uses the minimum supported Couchbase Server Enterprise and Sync Gateway Community versions defined in `ci/docker-up-down.py`. Run `./ci/docker-up-down.py --help` to see both version pairs. It starts the server and the legacy Sync Gateway database `scratch-30` on public port 4884 and admin port 4885. The collections gateway is opt in.

`./ci/build_and_run_tests.py --with-server-only` runs the ignored replication tests against `scratch-30`. Add `--with-valigrind` to run them under Valgrind. The ordering test uses 10,000 documents by default; set `SG_TEST_DOCUMENT_COUNT=100` for a quick local check.

The `chat-stress` job uses `--stress-only`, which starts the same two services without creating the test user. Its driver provisions its own user through the admin API. Ordinary `up` creates `sguser/password` for the replication tests.

To run the maximum supported pair used by the scheduled `network-tests` matrix, pass `--max-supported-server-components-versions` to `up` and `down`. The flag takes precedence over version and package URL environment overrides. Without the flag, `COUCHBASE_VERSION` and `SG_VERSION` can override the defaults for a custom pair, and `SG_LEGACY_DEB_AMD64` and `SG_LEGACY_DEB_ARM64` can override the legacy package URL.

```sh
./ci/docker-up-down.py up --max-supported-server-components-versions
./ci/build_and_run_tests.py --with-server-only
./ci/docker-up-down.py down --max-supported-server-components-versions
```

For manual collection testing with Server 7+ and Sync Gateway 3.1+, add `--with-collections` to `up` and `down`. This starts a second Sync Gateway database, `scratch`, with collections `flowers.roses`, `flowers.tulips`, and `flowers.lavenders` on public port 4984 and admin port 4985. `SG_DEB_AMD64` and `SG_DEB_ARM64` can override that gateway's package URL. The default minimum pair cannot run this service.

The Rust native build applies `couchbase-lite-core-sys/patches/fleece-shared-buffer.patch` to the Core source checkout before compiling it. This fixes a zero-length Fleece slice allocation that Valgrind detects as an invalid write. `ci/valgrind-suppressions.supp` limits suppression of observed HTTP header warnings to WebSocket handshake paths.
