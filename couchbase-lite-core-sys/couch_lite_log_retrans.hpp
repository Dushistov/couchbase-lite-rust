#pragma once

#include "c4Base.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef void (*C4LogCallbackR)(C4LogDomain, C4LogLevel, const char *msg C4NONNULL);

void c4log_setRustCallback(C4LogLevel level, C4LogCallbackR callback) C4API;

#ifdef __cplusplus
}
#endif
