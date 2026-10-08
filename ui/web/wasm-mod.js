import { bootstrap } from "./rustradio-ui-bootstrap.js";

try {
  await bootstrap({
    pkgName: "sparslog_ui",
    wasmMemoryConfig: { initial: 64, maximum: 16384, shared: true },
    workerThreadStackSize: 1024 * 1024,
  });
} catch (error) {
  if (globalThis.document) {
    document.getElementById("status").textContent = `Startup failed: ${error}`;
  }
  throw error;
}
