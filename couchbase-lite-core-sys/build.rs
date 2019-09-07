use std::path::Path;

fn main() {
    let dst = cmake::Config::new(Path::new("couchbase-lite-core"))
        .define("DISABLE_LTO_BUILD", "True")
        .build_target("LiteCore")
        .build()
        .join("build");
}
