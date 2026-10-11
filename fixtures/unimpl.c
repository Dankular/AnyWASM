/* Calls a kernel32 function the runtime does not implement, to test explicit failure reporting. */
#include <windows.h>
#include <tlhelp32.h>
#include <stdio.h>
int main(void) {
    printf("before\n");
    HANDLE h = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    printf("after %p\n", (void *)h);
    return 0;
}
