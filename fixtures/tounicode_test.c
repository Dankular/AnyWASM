/* Guest regression for common US-layout ToUnicodeW keyboard translations. */
typedef unsigned int UINT;
typedef unsigned short WCHAR;

__declspec(dllimport) int __stdcall ToUnicode(
    UINT virtual_key, UINT scan_code, const unsigned char *key_state,
    WCHAR *buffer, int capacity, UINT flags);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

void mainCRTStartup(void)
{
    unsigned char key_state[256];
    WCHAR buffer[4];
    int result;
    unsigned int i;

    for (i = 0; i < 256; ++i)
        key_state[i] = 0;
    for (i = 0; i < 4; ++i)
        buffer[i] = 0;

    result = ToUnicode(0x41, 0x1e, key_state, buffer, 4, 4);
    if (result != 1 || buffer[0] != 'a')
        ExitProcess(10);

    key_state[0x10] = 0x80; /* VK_SHIFT down */
    result = ToUnicode(0x41, 0x1e, key_state, buffer, 4, 4);
    if (result != 1 || buffer[0] != 'A')
        ExitProcess(11);

    result = ToUnicode(0x31, 0x02, key_state, buffer, 4, 4);
    if (result != 1 || buffer[0] != '!')
        ExitProcess(12);

    key_state[0x10] = 0;
    result = ToUnicode(0x20, 0x39, key_state, buffer, 4, 4);
    if (result != 1 || buffer[0] != ' ')
        ExitProcess(13);

    result = ToUnicode(0x25, 0x4b, key_state, buffer, 4, 4);
    if (result != 0)
        ExitProcess(14);

    ExitProcess(0);
}
