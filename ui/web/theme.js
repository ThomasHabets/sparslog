// Run before the page is painted; storage may be unavailable in private browsing.
(() => {
  const key = "sparslog-theme";
  const choices = ["system", "light", "dark"];
  const system = window.matchMedia("(prefers-color-scheme: dark)");
  let choice = "system";
  try {
    const saved = localStorage.getItem(key);
    if (choices.includes(saved)) choice = saved;
  } catch { /* Keep the system default when storage is unavailable. */ }

  function apply() {
    const theme = choice === "system" ? (system.matches ? "dark" : "light") : choice;
    document.documentElement.dataset.theme = theme;
    document.querySelector('meta[name="theme-color"]').content =
      theme === "dark" ? "#111a17" : "#f4f6f5";
  }
  apply();
  system.addEventListener("change", apply);
  document.addEventListener("DOMContentLoaded", () => {
    const selector = document.getElementById("theme");
    selector.value = choice;
    selector.addEventListener("change", () => {
      if (!choices.includes(selector.value)) return;
      choice = selector.value;
      try { localStorage.setItem(key, choice); } catch { /* Selection still works. */ }
      apply();
    });
  });
})();
