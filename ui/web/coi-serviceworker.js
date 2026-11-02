/* Add the COOP/COEP headers required by shared WASM memory on GitHub Pages. */
if (typeof window === "undefined") {
  self.addEventListener("install", () => self.skipWaiting());
  self.addEventListener("activate", (event) => {
    event.waitUntil(self.clients.claim());
  });
  self.addEventListener("fetch", (event) => {
    if (event.request.cache === "only-if-cached" && event.request.mode !== "same-origin") {
      return;
    }
    event.respondWith(
      fetch(event.request).then((response) => {
        if (response.status === 0) {
          return response;
        }
        const headers = new Headers(response.headers);
        headers.set("Cross-Origin-Embedder-Policy", "require-corp");
        headers.set("Cross-Origin-Opener-Policy", "same-origin");
        return new Response(response.body, {
          status: response.status,
          statusText: response.statusText,
          headers,
        });
      }),
    );
  });
} else {
  const scriptUrl = document.currentScript.src;
  globalThis.coiReady = (async () => {
    if (globalThis.crossOriginIsolated) {
      return;
    }
    if (!globalThis.isSecureContext || !("serviceWorker" in navigator)) {
      throw new Error("Cross-origin isolation requires HTTPS and service-worker support");
    }

    await navigator.serviceWorker.register(scriptUrl);
    if (!navigator.serviceWorker.controller) {
      await new Promise((resolve) => {
        navigator.serviceWorker.addEventListener("controllerchange", resolve, { once: true });
      });
    }
    globalThis.location.reload();
    await new Promise(() => {});
  })();
}
