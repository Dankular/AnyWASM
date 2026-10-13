/* Synthetic texture-heavy load used to profile the WebGPU executor (web/gpu.js): many large
 * textures with initial data, BGRA and mip-mapped textures, per-frame UpdateSubresource uploads,
 * a dynamic buffer refreshed every frame, and a draw per frame. It is a PROXY for a game's
 * loading phase, not a measurement of any real title. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>
#include <string.h>
#include "shaders_textured.h"

typedef struct { float x, y, z, u, v; } Vertex;
enum { N_RGBA = 96, RGBA_SIZE = 512, N_BGRA = 32, BGRA_SIZE = 256, N_MIP = 8, MIP_SIZE = 1024, FRAMES = 20, UPLOADS = 4 };
static unsigned char texels[MIP_SIZE * MIP_SIZE * 4];

static LRESULT CALLBACK WndProc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_DESTROY) { PostQuitMessage(0); return 0; }
    return DefWindowProcA(h, m, w, l);
}

int main(void) {
    for (unsigned i = 0; i < sizeof texels; i++) texels[i] = (unsigned char)(i * 131u + (i >> 9));
    WNDCLASSEXA wc = { sizeof wc };
    wc.lpfnWndProc = WndProc; wc.hInstance = GetModuleHandleA(NULL); wc.lpszClassName = "AnyWasmTexLoad";
    RegisterClassExA(&wc);
    HWND hwnd = CreateWindowExA(0, wc.lpszClassName, "texload", WS_OVERLAPPEDWINDOW, 0, 0, 640, 480, NULL, NULL, wc.hInstance, NULL);
    ShowWindow(hwnd, SW_SHOW);

    DXGI_SWAP_CHAIN_DESC sd = {0};
    sd.BufferCount = 1; sd.BufferDesc.Width = 640; sd.BufferDesc.Height = 480;
    sd.BufferDesc.Format = DXGI_FORMAT_R8G8B8A8_UNORM; sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.OutputWindow = hwnd; sd.SampleDesc.Count = 1; sd.Windowed = TRUE;
    ID3D11Device *dev; ID3D11DeviceContext *ctx; IDXGISwapChain *sc; D3D_FEATURE_LEVEL fl;
    HRESULT hr = D3D11CreateDeviceAndSwapChain(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, NULL, 0, D3D11_SDK_VERSION, &sd, &sc, &dev, &fl, &ctx);
    if (FAILED(hr)) return 1;
    ID3D11Texture2D *back; IDXGISwapChain_GetBuffer(sc, 0, &IID_ID3D11Texture2D, (void **)&back);
    ID3D11RenderTargetView *rtv; ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)back, NULL, &rtv);

    static ID3D11Texture2D *textures[N_RGBA + N_BGRA + N_MIP];
    static ID3D11ShaderResourceView *views[N_RGBA + N_BGRA + N_MIP];
    int n = 0, failed = 0;
    for (int i = 0; i < N_RGBA; i++, n++) {   /* immutable, initial data */
        D3D11_TEXTURE2D_DESC td = {0};
        td.Width = td.Height = RGBA_SIZE; td.MipLevels = 1; td.ArraySize = 1; td.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
        td.SampleDesc.Count = 1; td.Usage = D3D11_USAGE_IMMUTABLE; td.BindFlags = D3D11_BIND_SHADER_RESOURCE;
        D3D11_SUBRESOURCE_DATA sr = { texels, RGBA_SIZE * 4, 0 };
        failed += FAILED(ID3D11Device_CreateTexture2D(dev, &td, &sr, &textures[n]));
    }
    for (int i = 0; i < N_BGRA; i++, n++) {   /* a format that needs no swizzle on the CPU in WebGPU */
        D3D11_TEXTURE2D_DESC td = {0};
        td.Width = td.Height = BGRA_SIZE; td.MipLevels = 1; td.ArraySize = 1; td.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
        td.SampleDesc.Count = 1; td.Usage = D3D11_USAGE_IMMUTABLE; td.BindFlags = D3D11_BIND_SHADER_RESOURCE;
        D3D11_SUBRESOURCE_DATA sr = { texels, BGRA_SIZE * 4, 0 };
        failed += FAILED(ID3D11Device_CreateTexture2D(dev, &td, &sr, &textures[n]));
    }
    for (int i = 0; i < N_MIP; i++, n++) {    /* full mip chain generated on the GPU */
        D3D11_TEXTURE2D_DESC td = {0};
        td.Width = td.Height = MIP_SIZE; td.MipLevels = 0; td.ArraySize = 1; td.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
        td.SampleDesc.Count = 1; td.Usage = D3D11_USAGE_DEFAULT;
        td.BindFlags = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET; td.MiscFlags = D3D11_RESOURCE_MISC_GENERATE_MIPS;
        failed += FAILED(ID3D11Device_CreateTexture2D(dev, &td, NULL, &textures[n]));
        ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)textures[n], 0, NULL, texels, MIP_SIZE * 4, 0);
    }
    for (int i = 0; i < n; i++) {
        failed += FAILED(ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)textures[i], NULL, &views[i]));
        if (i >= N_RGBA + N_BGRA) ID3D11DeviceContext_GenerateMips(ctx, views[i]);
    }

    D3D11_SAMPLER_DESC smd = {0};
    smd.Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR; smd.AddressU = smd.AddressV = smd.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP;
    smd.MaxLOD = D3D11_FLOAT32_MAX; smd.ComparisonFunc = D3D11_COMPARISON_NEVER;
    ID3D11SamplerState *smp; failed += FAILED(ID3D11Device_CreateSamplerState(dev, &smd, &smp));

    Vertex quad[4] = { { -0.9f, 0.9f, 0.5f, 0, 0 }, { 0.9f, 0.9f, 0.5f, 1, 0 }, { 0.9f, -0.9f, 0.5f, 1, 1 }, { -0.9f, -0.9f, 0.5f, 0, 1 } };
    unsigned short idx[6] = { 0, 1, 2, 0, 2, 3 };
    D3D11_BUFFER_DESC vbd = { sizeof quad, D3D11_USAGE_DYNAMIC, D3D11_BIND_VERTEX_BUFFER, D3D11_CPU_ACCESS_WRITE, 0, 0 };
    D3D11_SUBRESOURCE_DATA vinit = { quad, 0, 0 };
    ID3D11Buffer *vb; failed += FAILED(ID3D11Device_CreateBuffer(dev, &vbd, &vinit, &vb));
    D3D11_BUFFER_DESC ibd = { sizeof idx, D3D11_USAGE_DEFAULT, D3D11_BIND_INDEX_BUFFER, 0, 0, 0 };
    D3D11_SUBRESOURCE_DATA iinit = { idx, 0, 0 };
    ID3D11Buffer *ib; failed += FAILED(ID3D11Device_CreateBuffer(dev, &ibd, &iinit, &ib));
    float mvp[16] = { 1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1 };
    D3D11_BUFFER_DESC cbd = { sizeof mvp, D3D11_USAGE_DEFAULT, D3D11_BIND_CONSTANT_BUFFER, 0, 0, 0 };
    D3D11_SUBRESOURCE_DATA cinit = { mvp, 0, 0 };
    ID3D11Buffer *cb; failed += FAILED(ID3D11Device_CreateBuffer(dev, &cbd, &cinit, &cb));
    ID3D11VertexShader *vs; ID3D11PixelShader *ps;
    failed += FAILED(ID3D11Device_CreateVertexShader(dev, tex_vs, sizeof tex_vs, NULL, &vs));
    failed += FAILED(ID3D11Device_CreatePixelShader(dev, tex_ps, sizeof tex_ps, NULL, &ps));
    D3D11_INPUT_ELEMENT_DESC ied[2] = {
        { "POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT, 0, 0,  D3D11_INPUT_PER_VERTEX_DATA, 0 },
        { "TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT,    0, 12, D3D11_INPUT_PER_VERTEX_DATA, 0 },
    };
    ID3D11InputLayout *il; failed += FAILED(ID3D11Device_CreateInputLayout(dev, ied, 2, tex_vs, sizeof tex_vs, &il));
    printf("textures=%d failed=%d\n", n, failed);

    int frames = 0, running = 1; MSG msg;
    while (running) {
        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) {
            if (msg.message == WM_QUIT) running = 0;
            TranslateMessage(&msg); DispatchMessageA(&msg);
        }
        if (!running) break;
        for (int u = 0; u < UPLOADS; u++)   /* streaming uploads */
            ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)textures[(frames * UPLOADS + u) % N_MIP + N_RGBA + N_BGRA], 0, NULL, texels, MIP_SIZE * 4, 0);
        D3D11_MAPPED_SUBRESOURCE ms;
        if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)vb, 0, D3D11_MAP_WRITE_DISCARD, 0, &ms))) {
            memcpy(ms.pData, quad, sizeof quad);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)vb, 0);
        }
        D3D11_VIEWPORT vp = { 0, 0, 640, 480, 0, 1 };
        ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, NULL);
        float clear[4] = { 0, 0, 0.2f, 1 };
        ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, clear);
        UINT stride = sizeof(Vertex), offset = 0;
        ID3D11DeviceContext_IASetInputLayout(ctx, il);
        ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vb, &stride, &offset);
        ID3D11DeviceContext_IASetIndexBuffer(ctx, ib, DXGI_FORMAT_R16_UINT, 0);
        ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
        ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
        ID3D11DeviceContext_VSSetConstantBuffers(ctx, 0, 1, &cb);
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &views[frames % n]);
        ID3D11DeviceContext_PSSetSamplers(ctx, 0, 1, &smp);
        ID3D11DeviceContext_DrawIndexed(ctx, 6, 0, 0);
        IDXGISwapChain_Present(sc, 1, 0);
        if (++frames == FRAMES) PostQuitMessage(0);
    }
    printf("frames=%d\n", frames);
    return failed ? 2 : 0;
}
