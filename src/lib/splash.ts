// The launch splash lives in src/app.html (app icon + shimmer, painted before
// any JS runs). The app fades it out once it is ready to show real content.

export function hideSplash(): void {
  if (typeof document === "undefined") return;
  const el = document.getElementById("vicuna-splash");
  if (!el || el.classList.contains("vs-hide")) return;
  el.classList.add("vs-hide");
  setTimeout(() => el.remove(), 320);
}
