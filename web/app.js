const state = {
  token: null,
  kind: "book",
  queue: [],
  running: false,
  currentRequest: null,
};

const $ = (selector) => document.querySelector(selector);
const queue = $("#queue");
const drop = $("#drop");
const files = $("#files");

async function pair(payload) {
  const response = await fetch("/api/session", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  const data = await response.json();
  if (!response.ok) throw new Error(data.error || "配对失败");
  state.token = data.token;
  $("#pairing").hidden = true;
  $("#workspace").hidden = false;
  $("#connection").textContent = "已连接";
}

$("#pair-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    $("#pair-error").textContent = "";
    await pair({ pin: $("#pin").value });
  } catch (error) {
    $("#pair-error").textContent = error.message;
  }
});

const bootstrap = location.hash.slice(1);
if (bootstrap) {
  history.replaceState(null, "", location.pathname);
  pair({ bootstrap }).catch(() => {});
}

document.querySelectorAll(".kind").forEach((button) => {
  button.addEventListener("click", () => {
    document.querySelectorAll(".kind").forEach((item) => item.classList.remove("active"));
    button.classList.add("active");
    state.kind = button.dataset.kind;
    $("#drop-title").textContent =
      state.kind === "book" ? "把书拖到这里" : "把壁纸拖到这里";
    files.accept =
      state.kind === "book"
        ? ".epub,.pdf,.djvu,.djv,.mobi,.azw3,.fb2,.cbz,.cbr,.txt"
        : "image/png,image/jpeg,image/webp";
  });
});

["dragenter", "dragover"].forEach((type) => {
  drop.addEventListener(type, (event) => {
    event.preventDefault();
    drop.classList.add("over");
  });
});

["dragleave", "drop"].forEach((type) => {
  drop.addEventListener(type, (event) => {
    event.preventDefault();
    drop.classList.remove("over");
  });
});

drop.addEventListener("drop", (event) => enqueue(event.dataTransfer.files));
files.addEventListener("change", () => enqueue(files.files));
document.addEventListener("paste", (event) => {
  const pasted = [...event.clipboardData.items]
    .map((item) => item.getAsFile())
    .filter(Boolean);
  if (pasted.length) enqueue(pasted);
});

function enqueue(list) {
  for (const file of list) {
    state.queue.push({
      file,
      kind: state.kind,
      status: "等待",
      progress: 0,
      cancelled: false,
    });
  }
  render();
  run();
}

function cancel(index) {
  const item = state.queue[index];
  item.cancelled = true;
  item.status = "已取消";
  if (item.request === state.currentRequest) state.currentRequest.abort();
  render();
}

function render() {
  queue.innerHTML = "";
  state.queue.forEach((item, index) => {
    const li = document.createElement("li");
    li.className = "item";
    li.innerHTML = `
      <div class="row"><span></span><small></small><button class="cancel">取消</button></div>
      <div class="progress"><i></i></div>`;
    li.querySelector("span").textContent = item.file.name;
    li.querySelector("small").textContent = item.status;
    li.querySelector("i").style.width = `${item.progress}%`;
    const cancelButton = li.querySelector(".cancel");
    cancelButton.hidden = !["等待", "上传中"].includes(item.status);
    cancelButton.addEventListener("click", () => cancel(index));
    queue.append(li);
  });
}

async function wallpaperPng(file) {
  if (file.type === "image/png") return file;
  const bitmap = await createImageBitmap(file);
  const canvas = document.createElement("canvas");
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  canvas.getContext("2d").drawImage(bitmap, 0, 0);
  const blob = await new Promise((resolve, reject) => {
    canvas.toBlob(
      (value) => (value ? resolve(value) : reject(new Error("图片转换失败"))),
      "image/png",
    );
  });
  bitmap.close();
  return new File([blob], file.name.replace(/\.[^.]+$/, "") + ".png", {
    type: "image/png",
  });
}

async function run() {
  if (state.running) return;
  state.running = true;
  for (const item of state.queue) {
    if (item.status !== "等待" || item.cancelled) continue;
    try {
      let file = item.file;
      if (item.kind === "wallpaper") file = await wallpaperPng(file);
      if (item.cancelled) continue;
      item.status = "上传中";
      render();
      const result = await send(file, item);
      item.status = `已保存为 ${result.saved_as}`;
      item.progress = 100;
    } catch (error) {
      if (!item.cancelled) item.status = error.message;
    }
    render();
  }
  state.running = false;
}

function send(file, item) {
  return new Promise((resolve, reject) => {
    const request = new XMLHttpRequest();
    item.request = request;
    state.currentRequest = request;
    request.open("PUT", "/api/upload");
    request.setRequestHeader("Authorization", `Bearer ${state.token}`);
    request.setRequestHeader("X-Upload-Kind", item.kind);
    request.setRequestHeader("X-Filename", encodeURIComponent(file.name));
    request.upload.onprogress = (event) => {
      if (event.lengthComputable) {
        item.progress = Math.round((event.loaded / event.total) * 100);
        render();
      }
    };
    request.onload = () => {
      state.currentRequest = null;
      let data = {};
      try {
        data = JSON.parse(request.responseText);
      } catch {}
      request.status === 201
        ? resolve(data)
        : reject(new Error(data.error || `上传失败 ${request.status}`));
    };
    request.onerror = () => {
      state.currentRequest = null;
      reject(new Error("网络连接中断"));
    };
    request.onabort = () => {
      state.currentRequest = null;
      reject(new Error("已取消"));
    };
    request.send(file);
  });
}
