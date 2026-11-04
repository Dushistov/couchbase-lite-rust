# Included by LiteCore's project() before its targets and subdirectories.
if(MSVC)
  # ParseDate.cc checks WIN32 rather than _WIN32.
  # Plus all libraries share one static image, so Fleece and LiteCore symbols must not be DLL imports.
  set_property(DIRECTORY APPEND PROPERTY COMPILE_DEFINITIONS
    WIN32 FLEECE_EXPORTS LITECORE_EXPORTS)
endif()

# A single target lets CMake resolve dependencies across subdirectories for
# every generator, including Visual Studio's separate FleeceStatic project.
# Defer it until LiteCore has finished applying compiler flags to its targets:
# a custom target has no sources and cannot accept target_compile_options().
cmake_language(DEFER CALL add_custom_target CouchbaseLiteRust)
cmake_language(DEFER CALL add_dependencies CouchbaseLiteRust
  LiteCoreStatic FleeceStatic BLIPStatic)
if(COUCHBASE_LITE_RUST_WEBSOCKET)
  cmake_language(DEFER CALL add_dependencies CouchbaseLiteRust LiteCoreWebSocket)
endif()
