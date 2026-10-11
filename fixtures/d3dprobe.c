/* Imports a D3D11 + user32 surface so the analyzer has a graphics API to detect. */
typedef void *HANDLE;
__declspec(dllimport) void __stdcall ExitProcess(unsigned);
__declspec(dllimport) int __stdcall MessageBoxA(void *, const char *, const char *, unsigned);
__declspec(dllimport) long __stdcall D3D11CreateDevice(void *, int, void *, unsigned, const int *, unsigned, unsigned, void **, int *, void **);
void mainCRTStartup(void) {
    void *dev = 0, *ctx = 0; int fl = 0;
    long hr = D3D11CreateDevice(0, 1, 0, 0, 0, 0, 7, &dev, &fl, &ctx);
    MessageBoxA(0, hr == 0 ? "ok" : "no d3d", "probe", 0);
    ExitProcess(0);
}
