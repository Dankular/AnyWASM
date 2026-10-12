/* Minimal Direct3D 11 program: window, device + swap chain, vertex buffer, constant buffer,
 * shaders from precompiled DXBC, draws a triangle for a few frames. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>
#include <string.h>
#include "shaders_triangle.h"

typedef struct { float x, y, z; float r, g, b, a; } Vertex;

static void check_staging_r32_float(ID3D11Device *dev, ID3D11DeviceContext *ctx) {
    D3D11_TEXTURE2D_DESC desc = {0};
    desc.Width = 2; desc.Height = 2; desc.MipLevels = 1; desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_R32_FLOAT; desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_DEFAULT;
    ID3D11Texture2D *source = NULL, *staging = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(dev, &desc, NULL, &source);
    if (FAILED(hr)) { printf("staging R32_FLOAT source hr=%08lx\n", (unsigned long)hr); return; }
    const float expected[4] = { 0.25f, 0.5f, 0.75f, 1.0f };
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)source, 0, NULL, expected, 2 * sizeof(float), sizeof expected);
    desc.Usage = D3D11_USAGE_STAGING; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    hr = ID3D11Device_CreateTexture2D(dev, &desc, NULL, &staging);
    if (SUCCEEDED(hr)) {
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)source);
        D3D11_MAPPED_SUBRESOURCE mapped;
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped);
        if (SUCCEEDED(hr)) {
            const float *values = (const float *)mapped.pData;
            printf("staging R32_FLOAT readback=%.2f,%.2f,%.2f,%.2f row_pitch=%u\n",
                values[0], values[1], values[2], values[3], mapped.RowPitch);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        } else printf("staging R32_FLOAT Map hr=%08lx\n", (unsigned long)hr);
        ID3D11Texture2D_Release(staging);
    } else printf("staging R32_FLOAT staging hr=%08lx\n", (unsigned long)hr);
    ID3D11Texture2D_Release(source);
}

static void check_staging_rgba16_float(ID3D11Device *dev, ID3D11DeviceContext *ctx) {
    D3D11_TEXTURE2D_DESC desc = {0};
    desc.Width = 2; desc.Height = 1; desc.MipLevels = 1; desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_R16G16B16A16_FLOAT; desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_DEFAULT;
    ID3D11Texture2D *source = NULL, *staging = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(dev, &desc, NULL, &source);
    if (FAILED(hr)) { printf("staging RGBA16_FLOAT source hr=%08lx\n", (unsigned long)hr); return; }
    const unsigned short expected[8] = { 0x3400, 0x3800, 0x3c00, 0x4000, 0, 0, 0, 0 };
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)source, 0, NULL, expected, sizeof expected, sizeof expected);
    desc.Usage = D3D11_USAGE_STAGING; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    hr = ID3D11Device_CreateTexture2D(dev, &desc, NULL, &staging);
    if (SUCCEEDED(hr)) {
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)source);
        D3D11_MAPPED_SUBRESOURCE mapped;
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped);
        if (SUCCEEDED(hr)) {
            const unsigned short *values = (const unsigned short *)mapped.pData;
            printf("staging RGBA16_FLOAT readback=%04x,%04x,%04x,%04x row_pitch=%u\n",
                values[0], values[1], values[2], values[3], mapped.RowPitch);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        } else printf("staging RGBA16_FLOAT Map hr=%08lx\n", (unsigned long)hr);
        ID3D11Texture2D_Release(staging);
    } else printf("staging RGBA16_FLOAT staging hr=%08lx\n", (unsigned long)hr);
    ID3D11Texture2D_Release(source);
}

static void check_staging_r8_subresources(ID3D11Device *dev, ID3D11DeviceContext *ctx) {
    static const BYTE mip0_slice0[6] = { 11, 12, 13, 21, 22, 23 };
    static const BYTE mip1_slice0[1] = { 31 };
    static const BYTE mip0_slice1[6] = { 41, 42, 43, 51, 52, 53 };
    static const BYTE mip1_slice1[1] = { 61 };
    D3D11_SUBRESOURCE_DATA init[4] = {0};
    init[0].pSysMem = mip0_slice0; init[0].SysMemPitch = 3; init[0].SysMemSlicePitch = 6;
    init[1].pSysMem = mip1_slice0; init[1].SysMemPitch = 1; init[1].SysMemSlicePitch = 1;
    init[2].pSysMem = mip0_slice1; init[2].SysMemPitch = 3; init[2].SysMemSlicePitch = 6;
    init[3].pSysMem = mip1_slice1; init[3].SysMemPitch = 1; init[3].SysMemSlicePitch = 1;
    D3D11_TEXTURE2D_DESC desc = {0};
    desc.Width = 3; desc.Height = 2; desc.MipLevels = 2; desc.ArraySize = 2;
    desc.Format = DXGI_FORMAT_R8_UNORM; desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_DEFAULT;
    ID3D11Texture2D *source = NULL, *staging = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(dev, &desc, init, &source);
    if (FAILED(hr)) { printf("staging R8_UNORM source hr=%08lx\n", (unsigned long)hr); return; }
    desc.Usage = D3D11_USAGE_STAGING; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    hr = ID3D11Device_CreateTexture2D(dev, &desc, NULL, &staging);
    if (SUCCEEDED(hr)) {
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)source);
        D3D11_MAPPED_SUBRESOURCE mapped;
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped);
        if (SUCCEEDED(hr)) {
            const BYTE *values = (const BYTE *)mapped.pData;
            printf("staging R8_UNORM subresource0=%u,%u,%u,%u,%u,%u row_pitch=%u\n",
                values[0], values[1], values[2], values[3], values[4], values[5], mapped.RowPitch);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        } else printf("staging R8_UNORM subresource0 Map hr=%08lx\n", (unsigned long)hr);
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 3, D3D11_MAP_READ, 0, &mapped);
        if (SUCCEEDED(hr)) {
            const BYTE *value = (const BYTE *)mapped.pData;
            printf("staging R8_UNORM subresource3=%u row_pitch=%u\n", value[0], mapped.RowPitch);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 3);
        } else printf("staging R8_UNORM subresource3 Map hr=%08lx\n", (unsigned long)hr);
        ID3D11Texture2D_Release(staging);
    } else printf("staging R8_UNORM staging hr=%08lx\n", (unsigned long)hr);
    ID3D11Texture2D_Release(source);
}

static void check_staging_d32_float(ID3D11Device *dev, ID3D11DeviceContext *ctx) {
    D3D11_TEXTURE2D_DESC desc = {0};
    desc.Width = 2; desc.Height = 2; desc.MipLevels = 1; desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_D32_FLOAT; desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_DEFAULT; desc.BindFlags = D3D11_BIND_DEPTH_STENCIL;
    ID3D11Texture2D *source = NULL, *staging = NULL;
    ID3D11DepthStencilView *dsv = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(dev, &desc, NULL, &source);
    if (FAILED(hr)) { printf("staging D32_FLOAT source hr=%08lx\n", (unsigned long)hr); return; }
    hr = ID3D11Device_CreateDepthStencilView(dev, (ID3D11Resource *)source, NULL, &dsv);
    if (FAILED(hr)) {
        printf("staging D32_FLOAT DSV hr=%08lx\n", (unsigned long)hr);
        ID3D11Texture2D_Release(source);
        return;
    }
    desc.Usage = D3D11_USAGE_STAGING; desc.BindFlags = 0; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    hr = ID3D11Device_CreateTexture2D(dev, &desc, NULL, &staging);
    if (SUCCEEDED(hr)) {
        ID3D11DeviceContext_ClearDepthStencilView(ctx, dsv, D3D11_CLEAR_DEPTH, 0.375f, 0);
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)source);
        D3D11_MAPPED_SUBRESOURCE mapped;
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped);
        if (SUCCEEDED(hr)) {
            const float *values = (const float *)mapped.pData;
            const float *secondRow = (const float *)((const BYTE *)mapped.pData + mapped.RowPitch);
            printf("staging D32_FLOAT readback=%.3f,%.3f,%.3f,%.3f row_pitch=%u\n",
                values[0], values[1], secondRow[0], secondRow[1], mapped.RowPitch);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        } else printf("staging D32_FLOAT Map hr=%08lx\n", (unsigned long)hr);
        ID3D11Texture2D_Release(staging);
    } else printf("staging D32_FLOAT staging hr=%08lx\n", (unsigned long)hr);
    ID3D11DepthStencilView_Release(dsv);
    ID3D11Texture2D_Release(source);
}

static void check_staging_texture3d(ID3D11Device *dev, ID3D11DeviceContext *ctx) {
    static const float expected[8] = { 0.125f, 0.25f, 0.375f, 0.5f, 0.625f, 0.75f, 0.875f, 1.0f };
    D3D11_TEXTURE3D_DESC desc = {0};
    desc.Width = 2; desc.Height = 2; desc.Depth = 2; desc.MipLevels = 1;
    desc.Format = DXGI_FORMAT_R32_FLOAT; desc.Usage = D3D11_USAGE_DEFAULT;
    D3D11_SUBRESOURCE_DATA init = { expected, 2 * sizeof(float), 4 * sizeof(float) };
    ID3D11Texture3D *source = NULL, *staging = NULL;
    HRESULT hr = ID3D11Device_CreateTexture3D(dev, &desc, &init, &source);
    if (FAILED(hr)) { printf("staging Texture3D R32_FLOAT source hr=%08lx\n", (unsigned long)hr); return; }
    desc.Usage = D3D11_USAGE_STAGING; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    hr = ID3D11Device_CreateTexture3D(dev, &desc, NULL, &staging);
    if (SUCCEEDED(hr)) {
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)source);
        D3D11_MAPPED_SUBRESOURCE mapped;
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped);
        if (SUCCEEDED(hr)) {
            const float *values = (const float *)mapped.pData;
            printf("staging Texture3D R32_FLOAT readback=%.3f,%.3f,%.3f,%.3f,%.3f,%.3f,%.3f,%.3f row_pitch=%u depth_pitch=%u\n",
                values[0], values[1], values[2], values[3], values[4], values[5], values[6], values[7],
                mapped.RowPitch, mapped.DepthPitch);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        } else printf("staging Texture3D R32_FLOAT Map hr=%08lx\n", (unsigned long)hr);
        ID3D11Texture3D_Release(staging);
    } else printf("staging Texture3D R32_FLOAT staging hr=%08lx\n", (unsigned long)hr);
    ID3D11Texture3D_Release(source);
}

static void check_staging_bgra8(ID3D11Device *dev, ID3D11DeviceContext *ctx) {
    static const BYTE expected[16] = { 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160 };
    D3D11_TEXTURE2D_DESC desc = {0};
    desc.Width = 2; desc.Height = 2; desc.MipLevels = 1; desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM; desc.SampleDesc.Count = 1; desc.Usage = D3D11_USAGE_DEFAULT;
    D3D11_SUBRESOURCE_DATA init = { expected, 2 * 4, sizeof expected };
    ID3D11Texture2D *source = NULL, *staging = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(dev, &desc, &init, &source);
    if (FAILED(hr)) { printf("staging BGRA8_UNORM source hr=%08lx\n", (unsigned long)hr); return; }
    desc.Usage = D3D11_USAGE_STAGING; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    hr = ID3D11Device_CreateTexture2D(dev, &desc, NULL, &staging);
    if (SUCCEEDED(hr)) {
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)source);
        D3D11_MAPPED_SUBRESOURCE mapped;
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped);
        if (SUCCEEDED(hr)) {
            const BYTE *values = (const BYTE *)mapped.pData;
            printf("staging BGRA8_UNORM readback=");
            for (UINT i = 0; i < 16; i++) printf("%s%u", i ? "," : "", values[i]);
            printf(" row_pitch=%u\n", mapped.RowPitch);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        } else printf("staging BGRA8_UNORM Map hr=%08lx\n", (unsigned long)hr);
        ID3D11Texture2D_Release(staging);
    } else printf("staging BGRA8_UNORM staging hr=%08lx\n", (unsigned long)hr);
    ID3D11Texture2D_Release(source);
}

static void check_staging_r16_float(ID3D11Device *dev, ID3D11DeviceContext *ctx) {
    static const WORD expected[6] = { 0x3400, 0x3800, 0x3c00, 0x4000, 0x4200, 0x4400 };
    D3D11_TEXTURE2D_DESC desc = {0};
    desc.Width = 3; desc.Height = 2; desc.MipLevels = 1; desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_R16_FLOAT; desc.SampleDesc.Count = 1; desc.Usage = D3D11_USAGE_DEFAULT;
    D3D11_SUBRESOURCE_DATA init = { expected, 3 * sizeof(WORD), sizeof expected };
    ID3D11Texture2D *source = NULL, *staging = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(dev, &desc, &init, &source);
    if (FAILED(hr)) { printf("staging R16_FLOAT source hr=%08lx\n", (unsigned long)hr); return; }
    desc.Usage = D3D11_USAGE_STAGING; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    hr = ID3D11Device_CreateTexture2D(dev, &desc, NULL, &staging);
    if (SUCCEEDED(hr)) {
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)source);
        D3D11_MAPPED_SUBRESOURCE mapped;
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped);
        if (SUCCEEDED(hr)) {
            const WORD *values = (const WORD *)mapped.pData;
            printf("staging R16_FLOAT readback=");
            for (UINT i = 0; i < 6; i++) printf("%s%04x", i ? "," : "", values[i]);
            printf(" row_pitch=%u\n", mapped.RowPitch);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        } else printf("staging R16_FLOAT Map hr=%08lx\n", (unsigned long)hr);
        ID3D11Texture2D_Release(staging);
    } else printf("staging R16_FLOAT staging hr=%08lx\n", (unsigned long)hr);
    ID3D11Texture2D_Release(source);
}

static void check_staging_r10g10b10a2(ID3D11Device *dev, ID3D11DeviceContext *ctx) {
    static const UINT expected[4] = { 0x12345678, 0x9abcdef0, 0x55aa33cc, 0xffffffff };
    D3D11_TEXTURE2D_DESC desc = {0};
    desc.Width = 2; desc.Height = 2; desc.MipLevels = 1; desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_R10G10B10A2_UNORM; desc.SampleDesc.Count = 1; desc.Usage = D3D11_USAGE_DEFAULT;
    D3D11_SUBRESOURCE_DATA init = { expected, 2 * sizeof(UINT), sizeof expected };
    ID3D11Texture2D *source = NULL, *staging = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(dev, &desc, &init, &source);
    if (FAILED(hr)) { printf("staging R10G10B10A2_UNORM source hr=%08lx\n", (unsigned long)hr); return; }
    desc.Usage = D3D11_USAGE_STAGING; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    hr = ID3D11Device_CreateTexture2D(dev, &desc, NULL, &staging);
    if (SUCCEEDED(hr)) {
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)source);
        D3D11_MAPPED_SUBRESOURCE mapped;
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped);
        if (SUCCEEDED(hr)) {
            const UINT *values = (const UINT *)mapped.pData;
            printf("staging R10G10B10A2_UNORM readback=%08x,%08x,%08x,%08x row_pitch=%u\n",
                values[0], values[1], values[2], values[3], mapped.RowPitch);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        } else printf("staging R10G10B10A2_UNORM Map hr=%08lx\n", (unsigned long)hr);
        ID3D11Texture2D_Release(staging);
    } else printf("staging R10G10B10A2_UNORM staging hr=%08lx\n", (unsigned long)hr);
    ID3D11Texture2D_Release(source);
}

static void check_staging_rg8(ID3D11Device *dev, ID3D11DeviceContext *ctx) {
    static const BYTE expected[12] = { 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120 };
    D3D11_TEXTURE2D_DESC desc = {0};
    desc.Width = 3; desc.Height = 2; desc.MipLevels = 1; desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_R8G8_UNORM; desc.SampleDesc.Count = 1; desc.Usage = D3D11_USAGE_DEFAULT;
    D3D11_SUBRESOURCE_DATA init = { expected, 3 * 2, sizeof expected };
    ID3D11Texture2D *source = NULL, *staging = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(dev, &desc, &init, &source);
    if (FAILED(hr)) { printf("staging R8G8_UNORM source hr=%08lx\n", (unsigned long)hr); return; }
    desc.Usage = D3D11_USAGE_STAGING; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    hr = ID3D11Device_CreateTexture2D(dev, &desc, NULL, &staging);
    if (SUCCEEDED(hr)) {
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)source);
        D3D11_MAPPED_SUBRESOURCE mapped;
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped);
        if (SUCCEEDED(hr)) {
            const BYTE *values = (const BYTE *)mapped.pData;
            printf("staging R8G8_UNORM readback=");
            for (UINT i = 0; i < 12; i++) printf("%s%u", i ? "," : "", values[i]);
            printf(" row_pitch=%u\n", mapped.RowPitch);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        } else printf("staging R8G8_UNORM Map hr=%08lx\n", (unsigned long)hr);
        ID3D11Texture2D_Release(staging);
    } else printf("staging R8G8_UNORM staging hr=%08lx\n", (unsigned long)hr);
    ID3D11Texture2D_Release(source);
}

static LRESULT CALLBACK WndProc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_DESTROY) { PostQuitMessage(0); return 0; }
    return DefWindowProcA(h, m, w, l);
}

int main(void) {
    WNDCLASSEXA wc = { sizeof wc };
    wc.lpfnWndProc = WndProc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "AnyWasmD3D11";
    RegisterClassExA(&wc);
    HWND hwnd = CreateWindowExA(0, wc.lpszClassName, "triangle", WS_OVERLAPPEDWINDOW, 0, 0, 640, 480, NULL, NULL, wc.hInstance, NULL);
    ShowWindow(hwnd, SW_SHOW);

    DXGI_SWAP_CHAIN_DESC sd = {0};
    sd.BufferCount = 1;
    sd.BufferDesc.Width = 640; sd.BufferDesc.Height = 480;
    sd.BufferDesc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.OutputWindow = hwnd; sd.SampleDesc.Count = 1; sd.Windowed = TRUE;
    ID3D11Device *dev = NULL; ID3D11DeviceContext *ctx = NULL; IDXGISwapChain *sc = NULL;
    D3D_FEATURE_LEVEL fl;
    HRESULT hr = D3D11CreateDeviceAndSwapChain(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, NULL, 0, D3D11_SDK_VERSION, &sd, &sc, &dev, &fl, &ctx);
    printf("create device hr=%08lx feature_level=%x\n", (unsigned long)hr, (unsigned)fl);
    if (FAILED(hr)) return 1;

    ID3D11Texture2D *back = NULL;
    IDXGISwapChain_GetBuffer(sc, 0, &IID_ID3D11Texture2D, (void **)&back);
    ID3D11RenderTargetView *rtv = NULL;
    hr = ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)back, NULL, &rtv);
    printf("rtv hr=%08lx\n", (unsigned long)hr);

    D3D11_TEXTURE2D_DESC readDesc = {0};
    readDesc.Width = 640; readDesc.Height = 480;
    readDesc.MipLevels = 1; readDesc.ArraySize = 1;
    readDesc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    readDesc.SampleDesc.Count = 1;
    readDesc.Usage = D3D11_USAGE_STAGING;
    readDesc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    ID3D11Texture2D *readback = NULL;
    hr = ID3D11Device_CreateTexture2D(dev, &readDesc, NULL, &readback);
    printf("staging texture hr=%08lx\n", (unsigned long)hr);

    Vertex verts[3] = {
        {  0.0f,  0.6f, 0.0f, 1, 0, 0, 1 },
        {  0.6f, -0.6f, 0.0f, 0, 1, 0, 1 },
        { -0.6f, -0.6f, 0.0f, 0, 0, 1, 1 },
    };
    D3D11_BUFFER_DESC bd = { sizeof verts, D3D11_USAGE_DEFAULT, D3D11_BIND_VERTEX_BUFFER, 0, 0, 0 };
    D3D11_SUBRESOURCE_DATA init = { verts, 0, 0 };
    ID3D11Buffer *vb = NULL;
    hr = ID3D11Device_CreateBuffer(dev, &bd, &init, &vb);
    printf("vb hr=%08lx\n", (unsigned long)hr);

    float tint[4] = { 1, 1, 1, 1 };
    D3D11_BUFFER_DESC cbd = { 16, D3D11_USAGE_DYNAMIC, D3D11_BIND_CONSTANT_BUFFER, D3D11_CPU_ACCESS_WRITE, 0, 0 };
    D3D11_SUBRESOURCE_DATA cinit = { tint, 0, 0 };
    ID3D11Buffer *cb = NULL;
    ID3D11Device_CreateBuffer(dev, &cbd, &cinit, &cb);

    D3D11_BUFFER_DESC readbackBufferDesc = { sizeof tint, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, 0, 0 };
    ID3D11Buffer *readbackBuffer = NULL;
    hr = ID3D11Device_CreateBuffer(dev, &readbackBufferDesc, NULL, &readbackBuffer);
    printf("staging buffer hr=%08lx\n", (unsigned long)hr);

    ID3D11VertexShader *vs = NULL; ID3D11PixelShader *ps = NULL;
    hr = ID3D11Device_CreateVertexShader(dev, tri_vs, sizeof tri_vs, NULL, &vs);
    printf("vs hr=%08lx\n", (unsigned long)hr);
    hr = ID3D11Device_CreatePixelShader(dev, tri_ps, sizeof tri_ps, NULL, &ps);
    printf("ps hr=%08lx\n", (unsigned long)hr);

    D3D11_INPUT_ELEMENT_DESC ied[2] = {
        { "POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT,    0, 0,  D3D11_INPUT_PER_VERTEX_DATA, 0 },
        { "COLOR",    0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 12, D3D11_INPUT_PER_VERTEX_DATA, 0 },
    };
    ID3D11InputLayout *il = NULL;
    hr = ID3D11Device_CreateInputLayout(dev, ied, 2, tri_vs, sizeof tri_vs, &il);
    printf("layout hr=%08lx\n", (unsigned long)hr);

    int frames = 0, running = 1;
    MSG msg;
    while (running) {
        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) {
            if (msg.message == WM_QUIT) running = 0;
            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }
        if (!running) break;

        D3D11_MAPPED_SUBRESOURCE ms;
        if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)cb, 0, D3D11_MAP_WRITE_DISCARD, 0, &ms))) {
            float t[4] = { 1.0f, 1.0f, 1.0f, 1.0f };
            memcpy(ms.pData, t, sizeof t);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)cb, 0);
        }
        if (frames == 0 && readbackBuffer) {
            ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)readbackBuffer, (ID3D11Resource *)cb);
            D3D11_MAPPED_SUBRESOURCE mappedBuffer;
            hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)readbackBuffer, 0, D3D11_MAP_READ, 0, &mappedBuffer);
            if (SUCCEEDED(hr)) {
                const float *values = (const float *)mappedBuffer.pData;
                printf("staging buffer readback=%.0f,%.0f,%.0f,%.0f\n", values[0], values[1], values[2], values[3]);
                ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)readbackBuffer, 0);
            } else {
                printf("staging buffer Map hr=%08lx\n", (unsigned long)hr);
            }
        }
        if (frames == 0) {
            check_staging_r32_float(dev, ctx);
            check_staging_rgba16_float(dev, ctx);
            check_staging_r8_subresources(dev, ctx);
            check_staging_d32_float(dev, ctx);
            check_staging_texture3d(dev, ctx);
            check_staging_bgra8(dev, ctx);
            check_staging_r16_float(dev, ctx);
            check_staging_r10g10b10a2(dev, ctx);
            check_staging_rg8(dev, ctx);
        }
        D3D11_VIEWPORT vp = { 0, 0, 640, 480, 0, 1 };
        ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, NULL);
        float clear[4] = { 0.0f, 0.0f, 0.2f, 1.0f };
        ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, clear);
        UINT stride = sizeof(Vertex), offset = 0;
        ID3D11DeviceContext_IASetInputLayout(ctx, il);
        ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vb, &stride, &offset);
        ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
        ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
        ID3D11DeviceContext_VSSetConstantBuffers(ctx, 0, 1, &cb);
        ID3D11DeviceContext_Draw(ctx, 3, 0);
        if (frames == 0 && readback) {
            ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)readback, (ID3D11Resource *)back);
            D3D11_MAPPED_SUBRESOURCE mapped;
            hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)readback, 0, D3D11_MAP_READ, 0, &mapped);
            if (SUCCEEDED(hr)) {
                const BYTE *pixels = (const BYTE *)mapped.pData;
                const BYTE *corner = pixels;
                const BYTE *center = pixels + ((size_t)288 * 640 + 320) * 4;
                printf("staging readback corner=%u,%u,%u,%u center=%u,%u,%u,%u\n",
                    corner[0], corner[1], corner[2], corner[3],
                    center[0], center[1], center[2], center[3]);
                ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)readback, 0);
            } else {
                printf("staging readback Map hr=%08lx\n", (unsigned long)hr);
            }
        }
        IDXGISwapChain_Present(sc, 1, 0);
        if (++frames == 3) PostQuitMessage(0);
    }
    printf("frames=%d\n", frames);
    ID3D11Buffer_Release(vb);
    if (readbackBuffer) ID3D11Buffer_Release(readbackBuffer);
    if (readback) ID3D11Texture2D_Release(readback);
    ID3D11RenderTargetView_Release(rtv);
    return 0;
}
