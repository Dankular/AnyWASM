/* Guest regression for registering and releasing a virtual device notification. */
typedef unsigned int DWORD;
typedef void *HANDLE;

typedef struct {
    DWORD size;
    DWORD type;
    DWORD reserved;
    unsigned char class_guid[16];
    unsigned short name[1];
} DEVICE_INTERFACE_FILTER_W;

__declspec(dllimport) HANDLE __stdcall GetDesktopWindow(void);
__declspec(dllimport) HANDLE __stdcall RegisterDeviceNotificationW(
    HANDLE recipient, const void *filter, DWORD flags);
__declspec(dllimport) int __stdcall UnregisterDeviceNotification(HANDLE notification);
__declspec(dllimport) void __stdcall ExitProcess(unsigned int code);

void mainCRTStartup(void)
{
    DEVICE_INTERFACE_FILTER_W filter = {0};
    filter.size = sizeof(filter);
    filter.type = 5; /* DBT_DEVTYP_DEVICEINTERFACE */
    filter.name[0] = 0;
    HANDLE notification = RegisterDeviceNotificationW(GetDesktopWindow(), &filter, 0);
    if (!notification)
        ExitProcess(1);
    if (!UnregisterDeviceNotification(notification))
        ExitProcess(2);
    if (UnregisterDeviceNotification(notification))
        ExitProcess(3);
    ExitProcess(0);
}
