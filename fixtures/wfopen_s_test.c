/* Freestanding Win64 check for wide-path FILE/VFS operations. */
typedef struct _FILE FILE;
typedef unsigned long long size_t;

__declspec(dllimport) void __stdcall ExitProcess(unsigned);
__declspec(dllimport) int __cdecl _wfopen_s(FILE **, const unsigned short *, const unsigned short *);
__declspec(dllimport) size_t __cdecl fwrite(const void *, size_t, size_t, FILE *);
__declspec(dllimport) size_t __cdecl fread(void *, size_t, size_t, FILE *);
__declspec(dllimport) int __cdecl fclose(FILE *);

static const unsigned short path[] = { 'c', 'a', 'f', 0x00e9, '.', 't', 'x', 't', 0 };
static const unsigned short missing_path[] = { 'n', 'o', 'n', 'e', '.', 't', 'x', 't', 0 };
static const unsigned short write_mode[] = { 'w', 'b', 0 };
static const unsigned short append_mode[] = { 'a', 'b', 0 };
static const unsigned short read_mode[] = { 'r', 'b', 0 };
static const char initial[] = { 'v', 'f', 's', '!' };
static const char appended[] = { '!' };

void mainCRTStartup(void) {
    FILE *stream = 0;
    if (_wfopen_s(&stream, path, write_mode) != 0 || !stream)
        ExitProcess(1);
    if (fwrite(initial, 1, sizeof initial, stream) != sizeof initial)
        ExitProcess(2);
    if (fclose(stream) != 0)
        ExitProcess(3);

    stream = 0;
    if (_wfopen_s(&stream, path, append_mode) != 0 || !stream)
        ExitProcess(4);
    if (fwrite(appended, 1, sizeof appended, stream) != sizeof appended)
        ExitProcess(5);
    if (fclose(stream) != 0)
        ExitProcess(6);

    stream = 0;
    if (_wfopen_s(&stream, path, read_mode) != 0 || !stream)
        ExitProcess(7);
    char contents[5] = { 0, 0, 0, 0, 0 };
    if (fread(contents, 1, sizeof contents, stream) != sizeof contents)
        ExitProcess(8);
    if (contents[0] != 'v' || contents[1] != 'f' || contents[2] != 's' ||
        contents[3] != '!' || contents[4] != '!')
        ExitProcess(9);
    if (fclose(stream) != 0)
        ExitProcess(10);

    stream = (FILE *)1;
    if (_wfopen_s(&stream, missing_path, read_mode) != 2 || stream != 0)
        ExitProcess(11);

    ExitProcess(0);
}
