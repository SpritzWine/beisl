/* beisl landing page — no dependencies, works offline.
   Brand rule respected throughout: Beislrot is chrome; only the layer
   colours (chart.rs truth) appear inside the charts. */
(function () {
  "use strict";

  var reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

  /* ---------- theme toggle: auto -> light -> dark ---------- */

  var THEMES = ["auto", "light", "dark"];
  var toggle = document.getElementById("theme-toggle");

  function applyTheme(mode) {
    if (mode === "light" || mode === "dark") {
      document.documentElement.setAttribute("data-theme", mode);
    } else {
      document.documentElement.removeAttribute("data-theme");
    }
    if (toggle) {
      var label = { auto: "Auto", light: "Light", dark: "Dark" }[mode];
      toggle.textContent = "Theme: " + label;
      toggle.setAttribute(
        "aria-label",
        "Colour theme: " + label + ". Activate to switch."
      );
    }
  }

  var stored = null;
  try { stored = localStorage.getItem("beisl-theme"); } catch (e) {}
  var theme = THEMES.indexOf(stored) >= 0 ? stored : "auto";
  applyTheme(theme);

  if (toggle) {
    toggle.addEventListener("click", function () {
      theme = THEMES[(THEMES.indexOf(theme) + 1) % THEMES.length];
      try { localStorage.setItem("beisl-theme", theme); } catch (e) {}
      applyTheme(theme);
    });
  }

  /* ---------- shared tooltip ---------- */

  var tip = document.createElement("div");
  tip.className = "tooltip";
  tip.setAttribute("role", "presentation");
  tip.setAttribute("aria-hidden", "true");
  document.body.appendChild(tip);

  function showTip(html, x, y) {
    tip.innerHTML = html;
    tip.classList.add("show");
    var r = tip.getBoundingClientRect();
    var left = Math.min(Math.max(8, x + 14), window.innerWidth - r.width - 8);
    var top = y - r.height - 12;
    if (top < 8) top = y + 18;
    tip.style.left = left + "px";
    tip.style.top = top + "px";
  }
  function hideTip() { tip.classList.remove("show"); }

  /* ---------- the verdict bar ----------
     Layer colours are the app's own (chart.rs); the driver segment is
     vendor-coloured, exactly as in the results view. */

  var INK_DARK = "#2b2320"; // Wirtshausholz
  var INK_CREAM = "#f4ede4"; // Kaffeehauscreme

  var LAYERS = [
    { id: "kernel", name: "kernel", color: "#5a6777" },
    { id: "driver", name: "GPU driver", color: "#3a9188" }, // vendor-recoloured below
    { id: "game", name: "game / JIT", color: "#e0a030" },
    { id: "dxvk", name: "DXVK / vkd3d", color: "#8e6fd6" },
    { id: "wine", name: "wine", color: "#9c2b3b" }
  ];

  var VENDORS = {
    neutral: { color: "#3a9188", label: "vendor-neutral" },
    intel: { color: "#0068b5", label: "Intel" },
    amd: { color: "#ed1c24", label: "AMD" },
    nvidia: { color: "#76b900", label: "NVIDIA" }
  };

  /* "clean" is Beisl's first real capture (Blood West, DESIGN.md);
     the other two nights and all coverage figures are illustrative. */
  var SCENARIOS = {
    clean: {
      shares: { kernel: 45, driver: 19.6, game: 32, dxvk: 3, wine: 0.4 },
      coverage: { kernel: 100, driver: 97, game: 24, dxvk: 95, wine: 100 },
      attributed: 92,
      verdict:
        "Not the wine — 0.4%. The kernel poured heaviest, and the game’s own JIT drank most of the rest. Good runs are a result too; now you can prove one."
    },
    driver: {
      shares: { kernel: 9, driver: 57, game: 21, dxvk: 9, wine: 4 },
      coverage: { kernel: 100, driver: 34, game: 61, dxvk: 95, wine: 98 },
      attributed: 66,
      verdict:
        "The driver’s round — 57%, and only a third of it named. The issues panel points at the exact debuginfo to install; do that, and it names names."
    },
    wine: {
      shares: { kernel: 7, driver: 11, game: 24, dxvk: 13, wine: 45 },
      coverage: { kernel: 100, driver: 96, game: 58, dxvk: 95, wine: 96 },
      attributed: 88,
      verdict:
        "Tonight it’s on the house — wine at 45%, resolved to symbols. Not a vibe: a named function someone can actually go fix."
    }
  };

  var bar = document.getElementById("layerbar");
  var tabBody = document.getElementById("tab-body");
  var verdictLine = document.getElementById("verdict-text");
  var attributedNote = document.getElementById("attributed-note");
  var vendorSelect = document.getElementById("vendor-select");
  var scenarioBtns = Array.prototype.slice.call(
    document.querySelectorAll(".scenario-btn")
  );

  var currentScenario = "clean";
  var currentVendor = "neutral";

  // srgb relative luminance, for picking the in-segment label ink
  function luminance(hex) {
    var n = parseInt(hex.slice(1), 16);
    var c = [n >> 16 & 255, n >> 8 & 255, n & 255].map(function (v) {
      v /= 255;
      return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
    });
    return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
  }
  function contrast(l1, l2) {
    var hi = Math.max(l1, l2), lo = Math.min(l1, l2);
    return (hi + 0.05) / (lo + 0.05);
  }
  function labelInk(bg) {
    var lb = luminance(bg);
    return contrast(lb, luminance(INK_DARK)) >= contrast(lb, luminance(INK_CREAM))
      ? INK_DARK
      : INK_CREAM;
  }

  function layerColor(layer) {
    return layer.id === "driver" ? VENDORS[currentVendor].color : layer.color;
  }

  function fmtShare(v) {
    return (v < 1 ? v.toFixed(1) : Math.round(v)) + "%";
  }

  function buildBar() {
    if (!bar) return;
    LAYERS.forEach(function (layer) {
      var seg = document.createElement("button");
      seg.type = "button";
      seg.className = "seg";
      seg.dataset.layer = layer.id;
      var pct = document.createElement("span");
      pct.className = "pct";
      pct.setAttribute("aria-hidden", "true");
      seg.appendChild(pct);

      function onHover(ev) {
        var s = SCENARIOS[currentScenario];
        var share = s.shares[layer.id];
        var cov = s.coverage[layer.id];
        var name =
          layer.id === "driver" && currentVendor !== "neutral"
            ? "GPU driver (" + VENDORS[currentVendor].label + ")"
            : layer.name;
        showTip(
          "<b>" + name + "</b> — " + fmtShare(share) +
            " of attributed samples<br><span class='tt-dim'>" +
            cov + "% of its samples named</span>",
          ev.clientX || 0,
          ev.clientY || 0
        );
      }
      seg.addEventListener("mousemove", onHover);
      seg.addEventListener("mouseleave", hideTip);
      seg.addEventListener("focus", function () {
        var r = seg.getBoundingClientRect();
        onHover({ clientX: r.left + r.width / 2, clientY: r.top });
      });
      seg.addEventListener("blur", hideTip);
      bar.appendChild(seg);
    });
  }

  function render() {
    var s = SCENARIOS[currentScenario];
    if (bar) {
      Array.prototype.forEach.call(bar.children, function (seg) {
        var id = seg.dataset.layer;
        var layer = LAYERS.filter(function (l) { return l.id === id; })[0];
        var share = s.shares[id];
        var color = layerColor(layer);
        seg.style.flexGrow = String(share);
        seg.style.backgroundColor = color;
        var name =
          id === "driver" && currentVendor !== "neutral"
            ? "GPU driver (" + VENDORS[currentVendor].label + ")"
            : layer.name;
        seg.setAttribute(
          "aria-label",
          name + ": " + fmtShare(share) + " of attributed samples, " +
            s.coverage[id] + "% of them named"
        );
        var pct = seg.querySelector(".pct");
        // in-bar % is a courtesy duplicate; the tab below is authoritative
        pct.textContent = share >= 7 ? fmtShare(share) : "";
        pct.style.color = labelInk(color);
      });
    }
    if (tabBody) {
      tabBody.innerHTML = "";
      LAYERS.forEach(function (layer) {
        var color = layerColor(layer);
        var name =
          layer.id === "driver" && currentVendor !== "neutral"
            ? "GPU driver · " + VENDORS[currentVendor].label
            : layer.name;
        var tr = document.createElement("tr");
        tr.innerHTML =
          "<th scope='row'><span class='layer-name'><span class='chip' style='background:" +
          color + "'></span>" + name + "</span></th>" +
          "<td class='num'>" + fmtShare(s.shares[layer.id]) + "</td>" +
          "<td class='cov-cell'><span class='num'>" + s.coverage[layer.id] +
          "% named</span><span class='covmeter' aria-hidden='true'><i style='width:" +
          s.coverage[layer.id] + "%'></i></span></td>";
        tabBody.appendChild(tr);
      });
    }
    if (verdictLine) verdictLine.textContent = s.verdict;
    if (attributedNote) {
      attributedNote.textContent =
        s.attributed + "% of samples attributed to a module · shares are over attributed samples — the " +
        (100 - s.attributed) +
        "% gap is disclosed, never redistributed into a layer.";
    }
  }

  buildBar();
  render();

  scenarioBtns.forEach(function (btn) {
    btn.addEventListener("click", function () {
      currentScenario = btn.dataset.scenario;
      scenarioBtns.forEach(function (b) {
        b.setAttribute("aria-pressed", String(b === btn));
      });
      render();
    });
  });

  if (vendorSelect) {
    vendorSelect.addEventListener("change", function () {
      currentVendor = vendorSelect.value;
      render();
    });
  }

  /* ---------- frametime side-channel figure ----------
     One series, monochrome ink — no signature colour inside a chart. */

  (function frametimeFigure() {
    var svg = document.getElementById("frametime-svg");
    if (!svg) return;

    var X0 = 46, X1 = 548, Y0 = 258, Y1 = 18; // plot box in viewBox units
    var T_MAX = 30, MS_MAX = 240;

    function x(t) { return X0 + (t / T_MAX) * (X1 - X0); }
    function y(ms) { return Y0 - (ms / MS_MAX) * (Y0 - Y1); }

    // Deterministic pseudo-noise so the figure is identical on every load.
    function noise(i) {
      var v = Math.sin(i * 12.9898) * 43758.5453;
      return v - Math.floor(v);
    }

    var data = [];
    var N = 300; // 30 s at 10 samples/s
    for (var i = 0; i <= N; i++) {
      var t = (i / N) * T_MAX;
      var ms = 7.6 + noise(i) * 2.6; // healthy 60 fps baseline wobble
      // minor bumps
      if (Math.abs(t - 3.2) < 0.15) ms = 24;
      if (Math.abs(t - 12.6) < 0.15) ms = 31;
      if (Math.abs(t - 24.9) < 0.15) ms = 27;
      // the bookmarked spikes
      if (Math.abs(t - 7.1) < 0.1) ms = 221; // DESIGN.md's own example bookmark
      else if (Math.abs(t - 7.1) < 0.35) ms = 46;
      if (Math.abs(t - 18.4) < 0.1) ms = 96;
      else if (Math.abs(t - 18.4) < 0.3) ms = 33;
      data.push({ t: t, ms: ms });
    }

    var pts = data
      .map(function (d) { return x(d.t).toFixed(1) + "," + y(d.ms).toFixed(1); })
      .join(" ");
    var line = document.getElementById("frametime-line");
    if (line) line.setAttribute("points", pts);

    svg.addEventListener("mousemove", function (ev) {
      var rect = svg.getBoundingClientRect();
      var vx = ((ev.clientX - rect.left) / rect.width) * 560;
      var t = ((vx - X0) / (X1 - X0)) * T_MAX;
      if (t < 0 || t > T_MAX) { hideTip(); return; }
      var idx = Math.round((t / T_MAX) * N);
      var d = data[Math.max(0, Math.min(N, idx))];
      showTip(
        "<b>" + d.ms.toFixed(1) + " ms</b> <span class='tt-dim'>at " +
          d.t.toFixed(1) + " s</span>",
        ev.clientX,
        ev.clientY
      );
    });
    svg.addEventListener("mouseleave", hideTip);
  })();

  /* ---------- hero: the living meniscus ----------
     A frametime trace scrolls along the bottom of the hero like the
     surface of the pour in the icon. Pure atmosphere, cream on wood.
     Honours prefers-reduced-motion by drawing a single still frame. */

  (function heroTrace() {
    var canvas = document.getElementById("hero-trace");
    if (!canvas) return;
    var ctx = canvas.getContext("2d");
    if (!ctx) return;

    var W = 0, H = 0, dpr = 1;
    var values = [];
    var spikeIn = 140; // samples until the next hitch
    var tick = 0;

    function resize() {
      dpr = Math.min(window.devicePixelRatio || 1, 2);
      W = canvas.clientWidth;
      H = canvas.clientHeight;
      canvas.width = Math.max(1, Math.round(W * dpr));
      canvas.height = Math.max(1, Math.round(H * dpr));
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    }

    function nextValue() {
      tick++;
      var v = 0.16 + Math.sin(tick / 23) * 0.03 + (Math.random() - 0.5) * 0.05;
      spikeIn--;
      if (spikeIn <= 0) {
        v = 0.62 + Math.random() * 0.3; // the hitch
        spikeIn = 130 + Math.floor(Math.random() * 260);
      } else if (spikeIn === 129 || spikeIn === 259) {
        v = 0.34; // settle after a hitch
      }
      return v;
    }

    function seed() {
      values = [];
      var n = Math.ceil(W / 3) + 2;
      for (var i = 0; i < n; i++) values.push(nextValue());
    }

    function draw() {
      ctx.clearRect(0, 0, W, H);
      ctx.beginPath();
      for (var i = 0; i < values.length; i++) {
        var px = i * 3;
        var py = H - values[i] * H;
        if (i === 0) ctx.moveTo(px, py);
        else ctx.lineTo(px, py);
      }
      ctx.strokeStyle = "rgba(244, 237, 228, 0.30)";
      ctx.lineWidth = 1.6;
      ctx.lineJoin = "round";
      ctx.stroke();
      // a faint fill below the meniscus, like the pour
      ctx.lineTo(values.length * 3, H);
      ctx.lineTo(0, H);
      ctx.closePath();
      ctx.fillStyle = "rgba(244, 237, 228, 0.045)";
      ctx.fill();
    }

    var frame = 0;
    function loop() {
      frame++;
      if (frame % 3 === 0) { // ~20 samples/s: a stroll, not a sprint
        values.push(nextValue());
        while (values.length > Math.ceil(W / 3) + 2) values.shift();
        draw();
      }
      raf = requestAnimationFrame(loop);
    }

    var raf = null;
    function start() {
      resize();
      seed();
      draw();
      if (!reducedMotion.matches) raf = requestAnimationFrame(loop);
    }
    function stop() {
      if (raf) cancelAnimationFrame(raf);
      raf = null;
    }

    window.addEventListener("resize", function () {
      stop();
      start();
    });
    reducedMotion.addEventListener("change", function () {
      stop();
      start();
    });
    document.addEventListener("visibilitychange", function () {
      if (document.hidden) stop();
      else if (!reducedMotion.matches && !raf) raf = requestAnimationFrame(loop);
    });

    start();
  })();

  /* ---------- copy button ---------- */

  var copyBtn = document.getElementById("copy-cmd");
  if (copyBtn) {
    copyBtn.addEventListener("click", function () {
      var text = "beisl %command%";
      function done(ok) {
        copyBtn.textContent = ok ? "Copied" : "Select it manually";
        setTimeout(function () { copyBtn.textContent = "Copy"; }, 1800);
      }
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(
          function () { done(true); },
          function () { done(false); }
        );
      } else {
        done(false);
      }
    });
  }

  /* ---------- release notes: releasenotes.md, rendered client-side ----------
     Single source of truth: the file is a verbatim copy of the repo's
     RELEASENOTES.md, refreshed at each release (see page/README.md). Fetch is
     same-origin only — the page's no-external-requests rule holds; under
     file:// (or any fetch failure) the section keeps its fallback link. */

  /* md-render-start (extracted for the offline renderer test) */
  function mdInline(s) {
    return s
      .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
      .replace(/`([^`]+)`/g, "<code>$1</code>")
      .replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>")
      .replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, '<a href="$2" rel="external">$1</a>')
      .replace(/\*([^*]+)\*/g, "<em>$1</em>");
  }

  function mdRender(md) {
    /* The subset RELEASENOTES.md actually uses: #/## headings, ---, - lists
       (with indented continuation lines), paragraphs, and the inline set
       above. Anything fancier belongs in the repo file, not here. */
    var out = [], inList = false, para = [];
    function flushPara() {
      if (para.length) { out.push("<p>" + mdInline(para.join(" ")) + "</p>"); para = []; }
    }
    function closeList() { if (inList) { out.push("</ul>"); inList = false; } }
    md.split(/\r?\n/).forEach(function (line) {
      var h = line.match(/^(#{1,3})\s+(.*)$/);
      if (h) {
        flushPara(); closeList();
        var lvl = h[1].length + 2; /* # -> h3: the section already owns the h2 */
        out.push("<h" + lvl + ">" + mdInline(h[2]) + "</h" + lvl + ">");
        return;
      }
      if (/^---+\s*$/.test(line)) { flushPara(); closeList(); out.push("<hr>"); return; }
      if (/^[-*]\s+/.test(line)) {
        flushPara();
        if (!inList) { out.push("<ul>"); inList = true; }
        out.push("<li>" + mdInline(line.replace(/^[-*]\s+/, "")) + "</li>");
        return;
      }
      if (inList && /^\s+\S/.test(line)) {
        out[out.length - 1] =
          out[out.length - 1].replace(/<\/li>$/, " " + mdInline(line.trim()) + "</li>");
        return;
      }
      if (/^\s*$/.test(line)) { flushPara(); closeList(); return; }
      para.push(line.trim());
    });
    flushPara(); closeList();
    return out.join("\n");
  }
  /* md-render-end */

  var notesHost = document.getElementById("release-notes");
  if (notesHost && window.fetch && location.protocol !== "file:") {
    fetch("releasenotes.md")
      .then(function (r) {
        if (!r.ok) throw new Error("HTTP " + r.status);
        return r.text();
      })
      .then(function (md) {
        notesHost.innerHTML = mdRender(md);
        /* Two releases worth of height, then scroll: each release opens with
           an h3, so the 3rd h3's top is exactly the cut line. With <= 2
           releases the box stays uncapped (nothing to scroll to). */
        var heads = notesHost.querySelectorAll("h3");
        if (heads.length > 2) {
          notesHost.style.maxHeight = heads[2].offsetTop + "px";
          notesHost.classList.add("scrolls");
        }
      })
      .catch(function () { /* fallback link stays */ });
  }
})();
