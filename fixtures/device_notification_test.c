/* Guest regression for registering and releasing a virtual device notification. */
typedef unsigned int DWORD;
typedef void *HANDLE;

__declspec(dllimport) HANDLE __stdcall RegisterDeviceNotificationW(
    HANDLE recipient, const void *filter, DWORD flags);
__declspec(dllimport) int __stdcall UnregisterDeviceNotification(HANDLE notification);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

void mainCRTStartup(void)
{
    DWORD filter[8];
    filter[0] = sizeof(filter);
    HANDLE notification = RegisterDeviceNotificationW((HANDLE)1, filter, 1);
    if (!notification)
        ExitProcess(1);
    if (!UnregisterDeviceNotification(notification))
        ExitProcess(2);
    if (UnregisterDeviceNotification(notification))
        ExitProcess(3);
    ExitProcess(0);
}
