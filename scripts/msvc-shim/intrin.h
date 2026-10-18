#pragma once
/* _byteswap 由 clang 内建 */
#pragma intrinsic(_byteswap_ulong)
static inline unsigned long _byteswap_ulong(unsigned long v) { return __builtin_bswap32((unsigned)v); }
