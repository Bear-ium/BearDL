// Picks the download for the visitor's system and highlights it. Without this script the page
// still lists every download.
(function () {
  "use strict";

  var base = "https://github.com/Bear-ium/BearDL/releases/latest/download/";
  var builds = {
    "windows-x86_64": { file: "beardl-windows-x86_64.zip", title: "Download for Windows", detail: "x86_64, zip" },
    "macos-aarch64": { file: "beardl-macos-aarch64.tar.gz", title: "Download for macOS", detail: "Apple silicon, tar.gz" },
    "macos-x86_64": { file: "beardl-macos-x86_64.tar.gz", title: "Download for macOS", detail: "Intel, tar.gz" },
    "linux-x86_64": { file: "beardl-linux-x86_64.tar.gz", title: "Download for Linux", detail: "x86_64, tar.gz" }
  };

  function osFromUserAgent(ua) {
    if (/iPhone|iPad|iPod|Android/i.test(ua)) return "mobile";
    if (/Windows/i.test(ua)) return "windows";
    if (/Macintosh|Mac OS X/i.test(ua)) {
      // iPadOS reports itself as a Mac, but has a touch screen.
      return navigator.maxTouchPoints > 1 ? "mobile" : "macos";
    }
    if (/CrOS/i.test(ua)) return "other";
    if (/Linux|X11|BSD/i.test(ua)) return "linux";
    return "other";
  }

  function archFromUserAgent(ua) {
    if (/aarch64|arm64|armv8/i.test(ua)) return "aarch64";
    if (/x86_64|x64|Win64|WOW64|amd64/i.test(ua)) return "x86_64";
    return null;
  }

  // Chromium browsers can report the CPU architecture; others leave it unknown.
  function archFromHints() {
    var uad = navigator.userAgentData;
    if (!uad || !uad.getHighEntropyValues) return Promise.resolve(null);
    return uad.getHighEntropyValues(["architecture", "bitness"]).then(function (h) {
      if (h.architecture === "arm") return "aarch64";
      if (h.architecture === "x86" && h.bitness === "64") return "x86_64";
      return null;
    }, function () { return null; });
  }

  function showNote(id) {
    var note = document.getElementById(id);
    if (note) note.hidden = false;
  }

  function show(os, arch) {
    var key = os + "-" + arch;

    if (os === "macos" && arch === null) {
      // Safari and Firefox do not say which kind of Mac this is; most are Apple silicon now.
      key = "macos-aarch64";
      showNote("note-mac-intel");
    } else if (os === "windows" && arch === "aarch64") {
      key = "windows-x86_64";
      showNote("note-windows-arm");
    } else if ((os === "windows" || os === "linux") && arch === null) {
      key = os + "-x86_64";
    }

    var build = builds[key];
    if (!build) {
      if (os === "linux") showNote("note-linux-arm");
      if (os === "mobile") showNote("note-mobile");
      return;
    }

    document.getElementById("get").href = base + build.file;
    document.getElementById("get-title").textContent = build.title;
    document.getElementById("get-detail").textContent = build.detail;

    var parts = key.split("-");
    var rows = document.querySelectorAll(".platforms li");
    for (var i = 0; i < rows.length; i++) {
      if (rows[i].getAttribute("data-os") === parts[0] && rows[i].getAttribute("data-arch") === parts[1]) {
        rows[i].classList.add("yours");
      }
    }
    var steps = document.querySelector('details[data-os="' + parts[0] + '"]');
    if (steps) steps.open = true;
  }

  var ua = navigator.userAgent || "";
  var os = osFromUserAgent(ua);
  archFromHints().then(function (arch) {
    // A Mac's user agent always says "Intel", so it is only trusted from the hints above.
    show(os, arch || (os === "macos" ? null : archFromUserAgent(ua)));
  });
})();
