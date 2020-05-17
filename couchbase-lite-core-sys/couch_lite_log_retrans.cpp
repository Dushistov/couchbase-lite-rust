#include <atomic>

#include "couch_lite_log_retrans.hpp"


static std::atomic<C4LogCallbackR> g_rust_log_callback{nullptr};

static void rust_log_callback(C4LogDomain d, C4LogLevel l,
                              const char *fmt C4NONNULL, va_list) {
  C4LogCallbackR rust_log_callback = g_rust_log_callback.load(std::memory_order_relaxed);
  if (rust_log_callback != nullptr) {
    rust_log_callback(d, l, fmt);
  }
}

void c4log_setRustCallback(C4LogLevel level, C4LogCallbackR callback) noexcept {
  g_rust_log_callback.store(callback, std::memory_order_relaxed);
  c4log_writeToCallback(level, rust_log_callback, true);
}

