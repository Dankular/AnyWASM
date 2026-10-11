/* Direct3D 11: textured, copied, and half-blended quads with a depth buffer, point sampling, indexed draws.
 * Draws the NEAR quad first and the FAR quad second, so the overlap is only correct if the depth test works. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>
#include "shaders_textured.h"

typedef struct { float x, y, z, u, v; } Vertex;

static LRESULT CALLBACK WndProc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_DESTROY) { PostQuitMessage(0); return 0; }
    return DefWindowProcA(h, m, w, l);
}

#define QUAD(x0, y0, x1, y1, z, u0, v0, u1, v1) \
    { x0, y1, z, u0, v0 }, { x1, y1, z, u1, v0 }, { x1, y0, z, u1, v1 }, { x0, y0, z, u0, v1 }

int main(void) {
    WNDCLASSEXA wc = { sizeof wc };
    wc.lpfnWndProc = WndProc; wc.hInstance = GetModuleHandleA(NULL); wc.lpszClassName = "AnyWasmTex";
    RegisterClassExA(&wc);
    HWND hwnd = CreateWindowExA(0, wc.lpszClassName, "textured", WS_OVERLAPPEDWINDOW, 0, 0, 640, 480, NULL, NULL, wc.hInstance, NULL);
    ShowWindow(hwnd, SW_SHOW);

    DXGI_SWAP_CHAIN_DESC sd = {0};
    sd.BufferCount = 1; sd.BufferDesc.Width = 640; sd.BufferDesc.Height = 480;
    sd.BufferDesc.Format = DXGI_FORMAT_R8G8B8A8_UNORM; sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.OutputWindow = hwnd; sd.SampleDesc.Count = 1; sd.Windowed = TRUE;
    ID3D11Device *dev; ID3D11DeviceContext *ctx; IDXGISwapChain *sc; D3D_FEATURE_LEVEL fl;
    HRESULT hr = D3D11CreateDeviceAndSwapChain(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, NULL, 0, D3D11_SDK_VERSION, &sd, &sc, &dev, &fl, &ctx);
    printf("device hr=%08lx\n", (unsigned long)hr);
    if (FAILED(hr)) return 1;

    ID3D11Texture2D *back; IDXGISwapChain_GetBuffer(sc, 0, &IID_ID3D11Texture2D, (void **)&back);
    ID3D11RenderTargetView *rtv; ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)back, NULL, &rtv);

    D3D11_TEXTURE2D_DESC dd = {0};
    dd.Width = 640; dd.Height = 480; dd.MipLevels = 1; dd.ArraySize = 1; dd.Format = DXGI_FORMAT_D32_FLOAT;
    dd.SampleDesc.Count = 1; dd.Usage = D3D11_USAGE_DEFAULT; dd.BindFlags = D3D11_BIND_DEPTH_STENCIL;
    ID3D11Texture2D *depth; hr = ID3D11Device_CreateTexture2D(dev, &dd, NULL, &depth);
    printf("depth tex hr=%08lx\n", (unsigned long)hr);
    ID3D11DepthStencilView *dsv; hr = ID3D11Device_CreateDepthStencilView(dev, (ID3D11Resource *)depth, NULL, &dsv);
    printf("dsv hr=%08lx\n", (unsigned long)hr);

    /* 2x2 texture: red, green / blue, white. */
    unsigned texels[4] = { 0xff0000ffu, 0xff00ff00u, 0xffff0000u, 0xffffffffu }; /* ABGR in memory = R8G8B8A8 */
    D3D11_TEXTURE2D_DESC td = {0};
    td.Width = 2; td.Height = 2; td.MipLevels = 1; td.ArraySize = 1; td.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    td.SampleDesc.Count = 1; td.Usage = D3D11_USAGE_IMMUTABLE; td.BindFlags = D3D11_BIND_SHADER_RESOURCE;
    D3D11_SUBRESOURCE_DATA tinit = { texels, 8, 0 };
    ID3D11Texture2D *tex; hr = ID3D11Device_CreateTexture2D(dev, &td, &tinit, &tex);
    printf("texture hr=%08lx\n", (unsigned long)hr);
    ID3D11ShaderResourceView *srv; hr = ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)tex, NULL, &srv);
    printf("srv hr=%08lx\n", (unsigned long)hr);

    /* Half-alpha white texel for the SRC_ALPHA blend check over quad C's blue region. */
    unsigned halfWhite = 0x80ffffffu; /* RGBA bytes: 255,255,255,128 */
    D3D11_TEXTURE2D_DESC alphaDesc = td; alphaDesc.Width = alphaDesc.Height = 1;
    D3D11_SUBRESOURCE_DATA alphaInit = { &halfWhite, 4, 0 };
    ID3D11Texture2D *alphaTex; hr = ID3D11Device_CreateTexture2D(dev, &alphaDesc, &alphaInit, &alphaTex);
    printf("half-white texture hr=%08lx\n", (unsigned long)hr);
    ID3D11ShaderResourceView *alphaSrv;
    hr = ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)alphaTex, NULL, &alphaSrv);
    printf("half-white srv hr=%08lx\n", (unsigned long)hr);

    /* Copy into a magenta-initialized texture; quad D must show the source red after the copy. */
    unsigned poison[4] = { 0xffff00ffu, 0xffff00ffu, 0xffff00ffu, 0xffff00ffu };
    D3D11_TEXTURE2D_DESC copyDesc = td; copyDesc.Usage = D3D11_USAGE_DEFAULT;
    D3D11_SUBRESOURCE_DATA copyInit = { poison, 8, 0 };
    ID3D11Texture2D *copyTex; hr = ID3D11Device_CreateTexture2D(dev, &copyDesc, &copyInit, &copyTex);
    printf("copy destination hr=%08lx\n", (unsigned long)hr);
    if (FAILED(hr)) return 2;
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)copyTex, (ID3D11Resource *)tex);
    ID3D11ShaderResourceView *copySrv;
    hr = ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)copyTex, NULL, &copySrv);
    printf("copy srv hr=%08lx\n", (unsigned long)hr);
    if (FAILED(hr)) return 2;

    D3D11_SAMPLER_DESC smd = {0};
    smd.Filter = D3D11_FILTER_MIN_MAG_MIP_POINT; smd.AddressU = smd.AddressV = smd.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP;
    smd.MaxLOD = D3D11_FLOAT32_MAX; smd.ComparisonFunc = D3D11_COMPARISON_NEVER;
    ID3D11SamplerState *smp; hr = ID3D11Device_CreateSamplerState(dev, &smd, &smp);
    printf("sampler hr=%08lx\n", (unsigned long)hr);

    D3D11_DEPTH_STENCIL_DESC dsd = {0};
    dsd.DepthEnable = TRUE; dsd.DepthWriteMask = D3D11_DEPTH_WRITE_MASK_ALL; dsd.DepthFunc = D3D11_COMPARISON_LESS;
    ID3D11DepthStencilState *dss; hr = ID3D11Device_CreateDepthStencilState(dev, &dsd, &dss);
    printf("depth state hr=%08lx\n", (unsigned long)hr);

    D3D11_BLEND_DESC bld = {0};
    bld.RenderTarget[0].BlendEnable = TRUE;
    bld.RenderTarget[0].SrcBlend = D3D11_BLEND_BLEND_FACTOR;
    bld.RenderTarget[0].DestBlend = D3D11_BLEND_INV_BLEND_FACTOR;
    bld.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
    bld.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE;
    bld.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_ZERO;
    bld.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
    bld.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL;
    ID3D11BlendState *blend; hr = ID3D11Device_CreateBlendState(dev, &bld, &blend);
    printf("blend state hr=%08lx\n", (unsigned long)hr);
    D3D11_BLEND_DESC alphaBld = bld;
    alphaBld.RenderTarget[0].SrcBlend = D3D11_BLEND_SRC_ALPHA;
    alphaBld.RenderTarget[0].DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
    ID3D11BlendState *alphaBlend;
    hr = ID3D11Device_CreateBlendState(dev, &alphaBld, &alphaBlend);
    printf("alpha blend state hr=%08lx\n", (unsigned long)hr);

    /* near quad A (z 0.2) samples only the red texel, far quad B (z 0.8) only the green texel,
     * quad C (z 0.5) maps the whole texture. */
    Vertex verts[24] = {
        QUAD(-0.8f, -0.1f, 0.2f, 0.7f, 0.2f, 0.25f, 0.25f, 0.25f, 0.25f),
        QUAD(-0.2f, -0.1f, 0.8f, 0.7f, 0.8f, 0.75f, 0.25f, 0.75f, 0.25f),
        QUAD(-0.6f, -0.9f, 0.6f, -0.3f, 0.5f, 0.0f, 0.0f, 1.0f, 1.0f),
        QUAD(-0.95f, -0.1f, -0.825f, 0.7f, 0.3f, 0.25f, 0.25f, 0.25f, 0.25f),
        QUAD(0.825f, -0.1f, 0.95f, 0.7f, 0.3f, 0.25f, 0.25f, 0.25f, 0.25f),
        QUAD(-0.3f, -0.87f, -0.05f, -0.67f, 0.4f, 0.25f, 0.25f, 0.25f, 0.25f),
    };
    unsigned short idx[36];
    for (int q = 0; q < 6; q++) {
        unsigned short b = (unsigned short)(q * 4), *i = idx + q * 6;
        i[0] = b; i[1] = b + 1; i[2] = b + 2; i[3] = b; i[4] = b + 2; i[5] = b + 3;
    }
    D3D11_BUFFER_DESC vbd = { sizeof verts, D3D11_USAGE_DEFAULT, D3D11_BIND_VERTEX_BUFFER, 0, 0, 0 };
    D3D11_SUBRESOURCE_DATA vinit = { verts, 0, 0 };
    ID3D11Buffer *vb; ID3D11Device_CreateBuffer(dev, &vbd, &vinit, &vb);
    Vertex poisonVerts[24];
    for (int i = 0; i < 24; i++) {
        poisonVerts[i].x = 2.0f; poisonVerts[i].y = 2.0f; poisonVerts[i].z = 0.3f;
        poisonVerts[i].u = 0.25f; poisonVerts[i].v = 0.25f;
    }
    D3D11_BUFFER_DESC copyVbd = vbd;
    D3D11_SUBRESOURCE_DATA poisonInit = { poisonVerts, 0, 0 };
    ID3D11Buffer *copyVb; hr = ID3D11Device_CreateBuffer(dev, &copyVbd, &poisonInit, &copyVb);
    printf("copy vertex buffer hr=%08lx\n", (unsigned long)hr);
    if (FAILED(hr)) return 2;
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)copyVb, (ID3D11Resource *)vb);
    D3D11_BUFFER_DESC ibd = { sizeof idx, D3D11_USAGE_DEFAULT, D3D11_BIND_INDEX_BUFFER, 0, 0, 0 };
    D3D11_SUBRESOURCE_DATA iinit = { idx, 0, 0 };
    ID3D11Buffer *ib; ID3D11Device_CreateBuffer(dev, &ibd, &iinit, &ib);

    float mvp[16] = { 1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1 };
    D3D11_BUFFER_DESC cbd = { sizeof mvp, D3D11_USAGE_DEFAULT, D3D11_BIND_CONSTANT_BUFFER, 0, 0, 0 };
    D3D11_SUBRESOURCE_DATA cinit = { mvp, 0, 0 };
    ID3D11Buffer *cb; ID3D11Device_CreateBuffer(dev, &cbd, &cinit, &cb);

    ID3D11VertexShader *vs; ID3D11PixelShader *ps;
    hr = ID3D11Device_CreateVertexShader(dev, tex_vs, sizeof tex_vs, NULL, &vs); printf("vs hr=%08lx\n", (unsigned long)hr);
    hr = ID3D11Device_CreatePixelShader(dev, tex_ps, sizeof tex_ps, NULL, &ps); printf("ps hr=%08lx\n", (unsigned long)hr);
    D3D11_INPUT_ELEMENT_DESC ied[2] = {
        { "POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT, 0, 0,  D3D11_INPUT_PER_VERTEX_DATA, 0 },
        { "TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT,    0, 12, D3D11_INPUT_PER_VERTEX_DATA, 0 },
    };
    ID3D11InputLayout *il; hr = ID3D11Device_CreateInputLayout(dev, ied, 2, tex_vs, sizeof tex_vs, &il);
    printf("layout hr=%08lx\n", (unsigned long)hr);

    int frames = 0, running = 1; MSG msg;
    while (running) {
        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) {
            if (msg.message == WM_QUIT) running = 0;
            TranslateMessage(&msg); DispatchMessageA(&msg);
        }
        if (!running) break;
        D3D11_VIEWPORT vp = { 0, 0, 640, 480, 0, 1 };
        ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, dsv);
        ID3D11DeviceContext_OMSetDepthStencilState(ctx, dss, 0);
        float clear[4] = { 0.0f, 0.0f, 0.2f, 1.0f };
        ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, clear);
        ID3D11DeviceContext_ClearDepthStencilView(ctx, dsv, D3D11_CLEAR_DEPTH, 1.0f, 0);
        UINT stride = sizeof(Vertex), offset = 0;
        ID3D11DeviceContext_IASetInputLayout(ctx, il);
        ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vb, &stride, &offset);
        ID3D11DeviceContext_IASetIndexBuffer(ctx, ib, DXGI_FORMAT_R16_UINT, 0);
        ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
        ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
        ID3D11DeviceContext_VSSetConstantBuffers(ctx, 0, 1, &cb);
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &srv);
        ID3D11DeviceContext_PSSetSamplers(ctx, 0, 1, &smp);
        float blendFactor[4] = { 0.5f, 0.5f, 0.5f, 0.5f };
        ID3D11DeviceContext_OMSetBlendState(ctx, blend, blendFactor, 0xffffffffu);
        ID3D11DeviceContext_DrawIndexed(ctx, 6, 0, 0);   /* A (near) */
        ID3D11DeviceContext_OMSetBlendState(ctx, NULL, NULL, 0xffffffffu);
        ID3D11DeviceContext_DrawIndexed(ctx, 6, 6, 0);   /* B (far, drawn later) */
        ID3D11DeviceContext_DrawIndexed(ctx, 6, 12, 0);  /* C */
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &copySrv);
        ID3D11DeviceContext_DrawIndexed(ctx, 6, 18, 0);  /* D (copied red texture) */
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &srv);
        ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &copyVb, &stride, &offset);
        ID3D11DeviceContext_DrawIndexed(ctx, 6, 24, 0);  /* E (copied vertex buffer) */
        ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vb, &stride, &offset);
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &alphaSrv);
        ID3D11DeviceContext_OMSetBlendState(ctx, alphaBlend, blendFactor, 0xffffffffu);
        ID3D11DeviceContext_DrawIndexed(ctx, 6, 30, 0);  /* F: half-alpha white over C's blue texel */
        ID3D11DeviceContext_OMSetBlendState(ctx, NULL, NULL, 0xffffffffu);
        IDXGISwapChain_Present(sc, 1, 0);
        if (++frames == 3) PostQuitMessage(0);
    }
    printf("frames=%d\n", frames);
    return 0;
}
