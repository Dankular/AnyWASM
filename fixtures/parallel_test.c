#include <intrin.h>

/* Two guest threads count primes in disjoint ranges and add their counts into one shared total
 * with a locked instruction. Pure integer work, so a CPU worker can run each thread. Expected:
 * primes below 100000 = 9592 (thread 1: [0,50000) = 5133, thread 2: [50000,100000) = 4459). */
typedef unsigned int DWORD;
typedef unsigned long long u64;
typedef void *HANDLE;
typedef DWORD (__stdcall *ThreadProc)(void *);
__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) int __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) HANDLE __stdcall CreateThread(void *, unsigned long long, ThreadProc, void *, DWORD, DWORD *);
__declspec(dllimport) DWORD __stdcall WaitForSingleObject(HANDLE, DWORD);
__declspec(dllimport) DWORD __stdcall TlsAlloc(void);
__declspec(dllimport) void *__stdcall TlsGetValue(DWORD);
__declspec(dllimport) int __stdcall TlsSetValue(DWORD, void *);
__declspec(dllimport) DWORD __stdcall GetLastError(void);
__declspec(dllimport) void __stdcall SetLastError(DWORD);
__declspec(dllimport) void __stdcall ExitProcess(unsigned);

int _fltused = 0;
static volatile long long total;
static volatile DWORD per_thread[2];
static volatile DWORD tls_result[2];
static volatile DWORD last_error_result[2];
static volatile DWORD rendezvous_arrivals;
static volatile DWORD rendezvous_result[2];
static volatile DWORD stack_result[2];
static volatile u64 stack_address[2];
static DWORD tls_index;

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
static void out_hex(DWORD v) {
    char b[9]; int i = 8; b[i] = 0;
    if (!v) b[--i] = '0';
    while (v) {
        unsigned digit = v & 15;
        b[--i] = (char)(digit < 10 ? '0' + digit : 'a' + digit - 10);
        v >>= 4;
    }
    out("0x"); out(b + i);
}

static int is_prime(unsigned n) {
    if (n < 2) return 0;
    for (unsigned d = 2; d * d <= n; d++) if (n % d == 0) return 0;
    return 1;
}

static DWORD __stdcall count_range(void *arg) {
    unsigned index = (unsigned)(u64)arg;
    unsigned lo = index * 50000u, hi = lo + 50000u, count = 0;
    volatile DWORD stack_sentinel = 0x71000000u + index;
    DWORD expected_tls = index ? 0x2222 : 0x1111;
    DWORD expected_last = index ? 0x4444 : 0x3333;
    stack_address[index] = (u64)(void *)&stack_sentinel;
    _InterlockedExchangeAdd((volatile long *)&rendezvous_arrivals, 1);
    for (unsigned spin = 0; spin < 100000u; spin++) {
        if (rendezvous_arrivals >= 2) {
            rendezvous_result[index] = 1;
            break;
        }
    }
    if (!TlsSetValue(tls_index, (void *)(u64)expected_tls)) return 10 + index;
    SetLastError(expected_last);
    for (unsigned n = lo; n < hi; n++) count += is_prime(n);
    stack_result[index] = stack_sentinel == 0x71000000u + index;
    per_thread[index] = count;
    last_error_result[index] = GetLastError();
    tls_result[index] = (DWORD)(u64)TlsGetValue(tls_index);
    _InterlockedExchangeAdd64(&total, (long long)count);
    return 0;
}

void mainCRTStartup(void) {
    tls_index = TlsAlloc();
    if (tls_index == 0xffffffffu || !TlsSetValue(tls_index, (void *)0x5555)) ExitProcess(2);
    HANDLE t0 = CreateThread(0, 0, count_range, (void *)0, 0, 0);
    HANDLE t1 = CreateThread(0, 0, count_range, (void *)1, 0, 0);
    if (!t0 || !t1) ExitProcess(3);
    WaitForSingleObject(t0, 0xffffffffu);
    WaitForSingleObject(t1, 0xffffffffu);
    SetLastError(0x6666);
    DWORD main_last_error = GetLastError();
    DWORD main_tls = (DWORD)(u64)TlsGetValue(tls_index);
    DWORD rendezvous_total = rendezvous_result[0] + rendezvous_result[1];
    DWORD stack_total = stack_result[0] + stack_result[1];
    if (stack_address[0] == stack_address[1]) stack_total = 0;
    out("t0="); out_u(per_thread[0]);
    out(" t1="); out_u(per_thread[1]);
    out(" total="); out_u((u64)total);
    out(" tls0="); out_hex(tls_result[0]);
    out(" tls1="); out_hex(tls_result[1]);
    out(" last0="); out_hex(last_error_result[0]);
    out(" last1="); out_hex(last_error_result[1]);
    out(" mainTls="); out_hex(main_tls);
    out(" mainLast="); out_hex(main_last_error);
    out(" rendezvous="); out_u(rendezvous_total);
    out(" stack="); out_u(stack_total);
    out("\r\n");
    ExitProcess(total == 9592 &&
        tls_result[0] == 0x1111 && tls_result[1] == 0x2222 &&
        last_error_result[0] == 0x3333 && last_error_result[1] == 0x4444 &&
        main_tls == 0x5555 && main_last_error == 0x6666 ? 0 : 1);
}
