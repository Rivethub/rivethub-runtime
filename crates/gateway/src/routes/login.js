(function () {
  var status = document.getElementById("status");
  var found = /(?:^#|&)code=([A-Za-z0-9_-]+)/.exec(location.hash);
  var next = new URLSearchParams(location.search).get("next");
  var target = sameOriginPath(next) ? next : "/";
  history.replaceState(null, "", location.pathname + location.search);
  if (!found) {
    status.textContent = "Run `rh open` in a terminal to sign in.";
    return;
  }
  fetch("/api/auth/session", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ code: found[1] }),
  })
    .then(function (response) {
      if (!response.ok) {
        throw new Error(String(response.status));
      }
      return response.json();
    })
    .then(function (body) {
      localStorage.setItem("rh-csrf", body.csrf);
      location.replace(target);
    })
    .catch(function () {
      status.textContent =
        "Sign-in failed. A code works once and expires after 60 seconds. Run `rh open` again.";
    });

  function sameOriginPath(path) {
    return (
      typeof path === "string" &&
      path.charAt(0) === "/" &&
      path.charAt(1) !== "/" &&
      !/[\\\x00-\x1f]/.test(path) &&
      path.indexOf("://") === -1 &&
      path.split(/[?#]/)[0].split("/").indexOf("..") === -1
    );
  }
})();
