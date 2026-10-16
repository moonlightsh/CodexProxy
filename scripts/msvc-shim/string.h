#pragma once
static inline void *memcpy(void *d, const void *s, unsigned long n) { return __builtin_memcpy(d, s, n); }
static inline void *memset(void *d, int c, unsigned long n) { return __builtin_memset(d, c, n); }
static inline int memcmp(const void *a, const void *b, unsigned long n) { return __builtin_memcmp(a, b, n); }
