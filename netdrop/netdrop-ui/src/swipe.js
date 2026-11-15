// Директива v-swipe-dismiss: свайп строки влево/вправо для удаления.
// Работает с мышью и тачем (pointer events). Значение: { enabled, onDismiss }.
export const vSwipeDismiss = {
  mounted(el, binding) {
    el._sd = binding.value;
    el.style.touchAction = "pan-y";
    let sx = 0, sy = 0, dx = 0, drag = false, horiz = false;
    const down = (e) => {
      const cfg = el._sd || {};
      if (cfg.enabled === false || e.button > 0) return;
      drag = true; horiz = false; sx = e.clientX; sy = e.clientY; dx = 0;
      el.style.transition = "";
    };
    const move = (e) => {
      if (!drag) return;
      dx = e.clientX - sx;
      const dy = e.clientY - sy;
      if (!horiz) {
        if (Math.abs(dx) > 12 && Math.abs(dx) > Math.abs(dy) * 1.5) {
          horiz = true;
          el.style.userSelect = "none";
          try { el.setPointerCapture(e.pointerId); } catch {}
        } else if (Math.abs(dy) > 12) { drag = false; return; }
        else return;
      }
      e.preventDefault();
      el.style.transform = `translateX(${dx}px)`;
      el.style.opacity = String(Math.max(0.25, 1 - Math.abs(dx) / 260));
    };
    const up = () => {
      if (!drag) return;
      drag = false;
      if (!horiz) return;
      // свайп состоялся — гасим ближайший click, чтобы не открыть файл случайно
      const stop = (ev) => { ev.stopPropagation(); ev.preventDefault(); };
      el.addEventListener("click", stop, { capture: true, once: true });
      setTimeout(() => el.removeEventListener("click", stop, { capture: true }), 300);
      el.style.userSelect = "";
      const w = el.offsetWidth || 300;
      el.style.transition = "transform .18s ease, opacity .18s ease";
      if (Math.abs(dx) > Math.min(110, w * 0.35)) {
        el.style.transform = `translateX(${dx > 0 ? w : -w}px)`;
        el.style.opacity = "0";
        setTimeout(() => { (el._sd || {}).onDismiss?.(); }, 170);
      } else {
        el.style.transform = "";
        el.style.opacity = "";
      }
    };
    el.addEventListener("pointerdown", down);
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  },
  updated(el, binding) { el._sd = binding.value; },
};
