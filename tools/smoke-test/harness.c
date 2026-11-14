/*
 * SAPE smoke test harness (32-bit). NOT a benchmark.
 *
 * Mimics how gta_sa.exe reaches Direct3D 9 (static import of
 * d3d9.dll!Direct3DCreate9), loads sape.dll first so its IAT hook applies,
 * then exercises the paths where a redundant-state filter can go wrong:
 *   1. plain redundant SetRenderState / SetSamplerState / SetTextureStageState
 *   2. state-block recording (calls must NOT update the shadow state)
 *   3. IDirect3DStateBlock9::Apply (must invalidate the shadow state)
 *   4. IDirect3DDevice9::Reset (must invalidate the shadow state)
 * Every check compares against the device's real state via Get*.
 *
 * Build: i686-w64-mingw32-gcc -O2 harness.c -o harness.exe -ld3d9 -luser32 -lgdi32
 * Run:   wine harness.exe <path-to-sape.dll|none> [frames] [bench]
 *
 * "bench" adds a call-overhead micro-benchmark (ns per SetRenderState call,
 * redundant vs. changing values). Under Wine this measures wined3d, not the
 * Microsoft runtime or a real driver: use it to compare SAPE configurations
 * against each other, never as a game-performance number.
 */
#define COBJMACROS
#include <windows.h>
#include <d3d9.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;
#define CHECK(cond, ...) do { if (!(cond)) { failures++; printf("FAIL: " __VA_ARGS__); printf("\n"); } else { printf("ok:   " __VA_ARGS__); printf("\n"); } } while (0)

static DWORD rs(IDirect3DDevice9 *d, D3DRENDERSTATETYPE s) { DWORD v = 0xDEADBEEF; IDirect3DDevice9_GetRenderState(d, s, &v); return v; }

struct V { float x, y, z, rhw; DWORD c; };

int main(int argc, char **argv) {
    const char *dll = argc > 1 ? argv[1] : "sape.dll";
    int frames = argc > 2 ? atoi(argv[2]) : 300;

    int bench = argc > 3 && strcmp(argv[3], "bench") == 0;
    if (strcmp(dll, "none") != 0) {
        HMODULE m = LoadLibraryA(dll);
        printf("LoadLibrary(%s) = %p\n", dll, (void *)m);
        if (!m) return 2;
    } else {
        printf("baseline run: SAPE not loaded\n");
    }

    WNDCLASSA wc = {0};
    wc.lpfnWndProc = DefWindowProcA;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "sape_smoke";
    RegisterClassA(&wc);
    HWND w = CreateWindowA("sape_smoke", "sape smoke", WS_OVERLAPPEDWINDOW, 0, 0, 640, 480, NULL, NULL, wc.hInstance, NULL);

    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION); /* through the (hooked) IAT */
    if (!d3d) { printf("Direct3DCreate9 failed\n"); return 3; }

    D3DPRESENT_PARAMETERS pp = {0};
    pp.Windowed = TRUE;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    pp.BackBufferFormat = D3DFMT_UNKNOWN;
    pp.hDeviceWindow = w;
    pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;

    IDirect3DDevice9 *dev = NULL;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, w,
                                         D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
    printf("CreateDevice = 0x%08lx\n", hr);
    if (FAILED(hr)) return 4;

    /* --- 1. redundant calls are harmless and state stays correct ---------- */
    for (int i = 0; i < 100; i++) {
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, FALSE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
        IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MINFILTER, D3DTEXF_LINEAR);
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
    }
    CHECK(rs(dev, D3DRS_CULLMODE) == D3DCULL_NONE, "redundant SetRenderState keeps CULLMODE=NONE");
    { DWORD v = 0; IDirect3DDevice9_GetSamplerState(dev, 0, D3DSAMP_MINFILTER, &v); CHECK(v == D3DTEXF_LINEAR, "sampler state kept"); }

    /* --- 2. recording must not update the shadow ---------------------------- */
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, FALSE);
    IDirect3DStateBlock9 *sb = NULL;
    IDirect3DDevice9_BeginStateBlock(dev);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE); /* recorded, not applied */
    IDirect3DDevice9_EndStateBlock(dev, &sb);
    CHECK(rs(dev, D3DRS_ALPHABLENDENABLE) == FALSE, "recording did not change device state");
    /* A filter that wrongly cached the recorded TRUE would skip this call: */
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE);
    CHECK(rs(dev, D3DRS_ALPHABLENDENABLE) == TRUE, "SetRenderState after recording reached the device");
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, FALSE);

    /* --- 3. Apply must invalidate ------------------------------------------- */
    IDirect3DStateBlock9_Apply(sb); /* device: ALPHABLEND = TRUE, shadow believed FALSE */
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, FALSE); /* must NOT be skipped */
    CHECK(rs(dev, D3DRS_ALPHABLENDENABLE) == FALSE, "SetRenderState after StateBlock::Apply reached the device");

    /* CreateStateBlock + Capture + Apply path */
    IDirect3DDevice9_SetRenderState(dev, D3DRS_FOGENABLE, TRUE);
    IDirect3DStateBlock9 *sb2 = NULL;
    IDirect3DDevice9_CreateStateBlock(dev, D3DSBT_ALL, &sb2);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_FOGENABLE, FALSE);
    IDirect3DStateBlock9_Apply(sb2); /* device: FOG = TRUE */
    IDirect3DDevice9_SetRenderState(dev, D3DRS_FOGENABLE, FALSE);
    CHECK(rs(dev, D3DRS_FOGENABLE) == FALSE, "SetRenderState after CreateStateBlock/Apply reached the device");

    /* --- textures for resource tracking ------------------------------------- */
    for (int i = 0; i < 20; i++) {
        IDirect3DTexture9 *t = NULL;
        if (SUCCEEDED(IDirect3DDevice9_CreateTexture(dev, 256, 256, 0, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t, NULL)) && i % 2 == 0)
            IDirect3DTexture9_Release(t); /* keep 10 alive */
    }

    /* --- frame loop ----------------------------------------------------------- */
    struct V tri[3] = { {320, 50, 0.5f, 1, 0xffff0000}, {540, 400, 0.5f, 1, 0xff00ff00}, {100, 400, 0.5f, 1, 0xff0000ff} };
    for (int f = 0; f < frames; f++) {
        MSG msg;
        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) DispatchMessageA(&msg);
        IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0xff202020, 1.0f, 0);
        IDirect3DDevice9_BeginScene(dev);
        for (int i = 0; i < 50; i++) { /* RenderWare-like: re-set states per object */
            IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, FALSE);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, (i & 7) == 0);
            IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
            IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLELIST, 1, tri, sizeof(struct V));
        }
        IDirect3DDevice9_EndScene(dev);
        IDirect3DDevice9_Present(dev, NULL, NULL, NULL, NULL);
        if (f == frames / 2) { Sleep(80); } /* deliberate spike */
    }

    if (bench) {
        LARGE_INTEGER f, a, b;
        const int N = 2000000;
        QueryPerformanceFrequency(&f);
        for (int rep = 0; rep < 3; rep++) {
            QueryPerformanceCounter(&a);
            for (int i = 0; i < N; i++) IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, FALSE);
            QueryPerformanceCounter(&b);
            double redundant = (double)(b.QuadPart - a.QuadPart) * 1e9 / f.QuadPart / N;
            QueryPerformanceCounter(&a);
            for (int i = 0; i < N; i++) IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, i & 1);
            QueryPerformanceCounter(&b);
            double changing = (double)(b.QuadPart - a.QuadPart) * 1e9 / f.QuadPart / N;
            printf("bench rep %d: SetRenderState redundant %.1f ns/call, changing %.1f ns/call\n", rep, redundant, changing);
        }
    }

    /* --- 4. Reset must invalidate ------------------------------------------- */
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DStateBlock9_Release(sb);
    IDirect3DStateBlock9_Release(sb2);
    hr = IDirect3DDevice9_Reset(dev, &pp); /* resets CULLMODE to default CCW */
    printf("Reset = 0x%08lx\n", hr);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE); /* must NOT be skipped */
    CHECK(rs(dev, D3DRS_CULLMODE) == D3DCULL_NONE, "SetRenderState after Reset reached the device");

    for (int f = 0; f < 30; f++) {
        IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0xff202020, 1.0f, 0);
        IDirect3DDevice9_Present(dev, NULL, NULL, NULL, NULL);
    }

    IDirect3DDevice9_Release(dev);
    IDirect3D9_Release(d3d);
    printf("%s (%d failures)\n", failures ? "SMOKE TEST FAILED" : "SMOKE TEST PASSED", failures);
    return failures ? 1 : 0;
}
