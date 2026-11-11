import init from "/local_playlist.js";

init().then(() => document.getElementById("loading")?.remove()).catch((e) => {
  document.getElementById("loading").textContent = "Failed to start: " + e;
});
