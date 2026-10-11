/* Minimal Direct3D 11 program: window, device + swap chain, vertex buffer, constant buffer,
 * shaders from precompiled DXBC, draws a triangle for a few frames. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>
#include "shaders_triangle.h"

typedef struct { float x, y, z; float r, g, b, a; } Vertex;

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
        IDXGISwapChain_Present(sc, 1, 0);
        if (++frames == 3) PostQuitMessage(0);
    }
    printf("frames=%d\n", frames);
    ID3D11Buffer_Release(vb);
    ID3D11RenderTargetView_Release(rtv);
    return 0;
}
