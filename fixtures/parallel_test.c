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
__declspec(dllimport) void __stdcall ExitProcess(unsigned);

int _fltused = 0;
static volatile long long total;
static volatile DWORD per_thread[2];

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

static int is_prime(unsigned n) {
    if (n < 2) return 0;
    for (unsigned d = 2; d * d <= n; d++) if (n % d == 0) return 0;
    return 1;
}

static DWORD __stdcall count_range(void *arg) {
    unsigned index = (unsigned)(u64)arg;
    unsigned lo = index * 50000u, hi = lo + 50000u, count = 0;
    for (unsigned n = lo; n < hi; n++) count += is_prime(n);
    per_thread[index] = count;
    __sync_fetch_and_add(&total, (long long)count);
    return 0;
}

void mainCRTStartup(void) {
    HANDLE t0 = CreateThread(0, 0, count_range, (void *)0, 0, 0);
    HANDLE t1 = CreateThread(0, 0, count_range, (void *)1, 0, 0);
    if (!t0 || !t1) ExitProcess(2);
    WaitForSingleObject(t0, 0xffffffffu);
    WaitForSingleObject(t1, 0xffffffffu);
    out("t0="); out_u(per_thread[0]);
    out(" t1="); out_u(per_thread[1]);
    out(" total="); out_u((u64)total); out("\r\n");
    ExitProcess(total == 9592 ? 0 : 1);
}
