/* Guest-side C library routines that the host can replace via native hooks (see
 * Process::install_native_hooks). Each routine has the standard C signature, and the program
 * prints checksums so a hooked run can be compared byte-for-byte with an unhooked run. */
#include <stdarg.h>
typedef void *HANDLE;
typedef unsigned long DWORD;
typedef int BOOL;
typedef unsigned long long u64;
typedef unsigned long long size_t_;
__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) BOOL __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) void __stdcall ExitProcess(unsigned);

int _fltused = 0;
static void out(const char *s) {
    DWORD n = 0, w;
    while (s[n]) n++;
    WriteFile(GetStdHandle((DWORD)-11), s, n, &w, 0);
}
static void out_u(u64 v) {
    char b[24]; int i = 23; b[i] = 0;
    if (!v) b[--i] = '0';
    while (v) { b[--i] = '0' + v % 10; v /= 10; }
    out(b + i);
}

__declspec(dllexport) __attribute__((noinline)) size_t_ my_strlen(const char *s) {
    size_t_ n = 0;
    while (s[n]) n++;
    return n;
}
__declspec(dllexport) __attribute__((noinline)) void *my_memcpy(void *d, const void *s, size_t_ n) {
    unsigned char *dd = d; const unsigned char *ss = s;
    for (size_t_ i = 0; i < n; i++) dd[i] = ss[i];
    return d;
}
__declspec(dllexport) __attribute__((noinline)) void *my_memset(void *d, int c, size_t_ n) {
    unsigned char *dd = d;
    for (size_t_ i = 0; i < n; i++) dd[i] = (unsigned char)c;
    return d;
}
__declspec(dllexport) __attribute__((noinline)) int my_strcmp(const char *a, const char *b) {
    while (*a && *a == *b) { a++; b++; }
    return (unsigned char)*a - (unsigned char)*b;
}
__declspec(dllexport) __attribute__((noinline)) int my_memcmp(const void *a, const void *b, size_t_ n) {
    const unsigned char *x = a, *y = b;
    for (size_t_ i = 0; i < n; i++) if (x[i] != y[i]) return x[i] - y[i];
    return 0;
}

/* Placeholder body: only a hook gives this function real behaviour (an unhooked run reports -99). */
__declspec(dllexport) __attribute__((noinline)) int my_vsscanf(const char *buf, const char *fmt, va_list ap) {
    /* Volatile sinks make the arguments observable, so callers must really pass them; the volatile
     * return keeps the compiler from folding the call away. */
    static const void *volatile sink;
    static volatile int placeholder = -99;
    sink = buf; sink = fmt; sink = (const void *)ap;
    return placeholder;
}
static __attribute__((noinline)) int call_vs(const char *buf, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = my_vsscanf(buf, fmt, ap);
    va_end(ap);
    return r;
}
static void out_i(long long v) { if (v < 0) { out("-"); out_u((u64)(-v)); } else out_u((u64)v); }

static char src[4096], dst[4096], text[2048];

void mainCRTStartup(void) {
    for (int i = 0; i < 4096; i++) src[i] = (char)(i * 7 + 3);
    for (int i = 0; i < 2047; i++) text[i] = (char)('a' + (i * 5) % 26);
    text[2047] = 0;
    u64 sum = 0;
    for (int round = 0; round < 40; round++) {
        my_memset(dst, round, sizeof dst);
        my_memcpy(dst + 8, src + round, 2000);
        sum += my_strlen(text + round);
        sum += (u64)(unsigned)my_memcmp(dst + 8, src + round, 2000);
        text[100 + round] ^= 1;
        sum += (u64)(unsigned)(my_strcmp(text, text + round) & 0xffff);
        for (int i = 0; i < 4096; i += 97) sum = sum * 31 + (unsigned char)dst[i];
    }
    out("checksum="); out_u(sum); out("\r\n");
    int a = 0, x = 0; long long ll = 0; float f = 0; char name[16] = {0};
    int r = call_vs("12 abc 3.5 -7 0x1f", "%d %s %f %lld %x", &a, name, &f, &ll, &x);
    out("scan="); out_i(r);
    if (r > 0) {
        out(" a="); out_i(a); out(" name="); out(name); out(" f2="); out_i((long long)(f * 2));
        out(" ll="); out_i(ll); out(" x="); out_i(x);
    }
    out("\r\n");
    ExitProcess(0);
}
