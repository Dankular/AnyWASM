/* Guest regression for common USER32 rectangle helpers. */
typedef long BOOL;
typedef struct { long left, top, right, bottom; } RECT;

__declspec(dllimport) BOOL __stdcall OffsetRect(RECT *rect, int dx, int dy);
__declspec(dllimport) BOOL __stdcall IntersectRect(RECT *out, const RECT *a, const RECT *b);
__declspec(dllimport) BOOL __stdcall IsRectEmpty(const RECT *rect);
__declspec(dllimport) BOOL __stdcall PtInRect(const RECT *rect, long long point);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

void mainCRTStartup(void)
{
    RECT a = {0, 0, 20, 20};
    RECT b = {10, 10, 30, 30};
    RECT out;
    if (!OffsetRect(&a, 5, -5) || a.left != 5 || a.top != -5 || a.right != 25 || a.bottom != 15)
        ExitProcess(1);
    if (!IntersectRect(&out, &a, &b) || out.left != 10 || out.top != 10 || out.right != 25 || out.bottom != 15)
        ExitProcess(2);
    if (IsRectEmpty(&out) || !PtInRect(&out, ((long long)12 << 32) | 12))
        ExitProcess(3);
    out.right = out.left;
    if (!IsRectEmpty(&out))
        ExitProcess(4);
    ExitProcess(0);
}
