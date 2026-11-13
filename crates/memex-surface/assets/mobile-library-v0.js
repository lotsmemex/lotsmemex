(() => {
  const allowed = {
    theme: new Set(["light", "dark"]),
    font: new Set(["sans", "serif"]),
  };
  const query = new URLSearchParams(location.search);

  for (const [name, values] of Object.entries(allowed)) {
    try {
      const stored = localStorage.getItem(`memex-${name}`);
      if (!query.has(name) && values.has(stored)) document.body.dataset[name] = stored;
    } catch (_) {
      // Preferences remain available through ordinary GET links.
    }
  }

  const syncPreferences = () => {
    document.querySelectorAll("[data-pref]").forEach((link) => {
      const active = document.body.dataset[link.dataset.pref] === link.dataset.value;
      link.classList.toggle("active", active);
      if (active) link.setAttribute("aria-current", "true");
      else link.removeAttribute("aria-current");
    });
  };
  syncPreferences();

  document.querySelectorAll("[data-pref]").forEach((link) => {
    link.addEventListener("click", () => {
      const name = link.dataset.pref;
      const value = link.dataset.value;
      if (!allowed[name]?.has(value)) return;
      try { localStorage.setItem(`memex-${name}`, value); } catch (_) {}
    });
  });

  if (matchMedia("(pointer: fine)").matches) {
    document.querySelectorAll("[data-read-url]").forEach((row) => {
      let pending;
      row.addEventListener("click", (event) => {
        event.preventDefault();
        clearTimeout(pending);
        if (event.detail > 1) {
          location.assign(row.dataset.readUrl);
        } else {
          pending = setTimeout(() => location.assign(row.href), 220);
        }
      });
    });
  }

  const captureForm = document.querySelector("[data-capture-form]");
  if (!captureForm) return;
  const captureInput = captureForm.querySelector("[data-capture-input]");
  const captureSubmit = captureForm.querySelector("[data-capture-submit]");
  const captureStatus = captureForm.querySelector("[data-capture-status]");
  const maximumCaptureBytes = 16 * 1024;
  let submitting = false;
  let idempotencyKey;
  let keyedText;

  const exactKeys = (value, keys) => value !== null
    && typeof value === "object"
    && !Array.isArray(value)
    && Object.keys(value).sort().join("\u0000") === [...keys].sort().join("\u0000");

  const urlStartCount = (text) => {
    let count = 0;
    for (const scheme of ["http://", "https://"]) {
      let offset = 0;
      while ((offset = text.indexOf(scheme, offset)) !== -1) {
        count += 1;
        if (count > 1) return count;
        offset += scheme.length;
      }
    }
    return count;
  };

  const captureValidation = (text) => {
    if (new TextEncoder().encode(text).length > maximumCaptureBytes) {
      return "内容过长，请缩短后再提交。";
    }
    if (/[\u0000-\u001f\u007f-\u009f]/u.test(text)) {
      return "请使用不含换行或控制字符的一句话。";
    }
    const count = urlStartCount(text);
    if (count === 0) return "请输入一句包含一个 HTTP(S) 链接的话。";
    if (count > 1) return "一次只能提交一个链接。";
    return undefined;
  };

  const newIdempotencyKey = () => {
    if (typeof crypto.randomUUID === "function") {
      return `local_${crypto.randomUUID()}`;
    }
    if (typeof crypto.getRandomValues !== "function") throw new Error("random_unavailable");
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    return `local_${Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("")}`;
  };

  const captureResponse = (value) => {
    if (!exactKeys(value, ["schema_version", "job_id", "state", "redacted_url"])
      || value.schema_version !== 1
      || !new Set(["queued", "parsing", "fetching", "validating", "storing", "needs_user", "failed", "stored"]).has(value.state)
      || typeof value.job_id !== "string"
      || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(value.job_id)
      || !exactKeys(value.redacted_url, ["host", "path_class", "query_present"])
      || typeof value.redacted_url.host !== "string"
      || value.redacted_url.host.length < 1
      || value.redacted_url.host.length > 255
      || !/^[A-Za-z0-9.:[\]-]+$/.test(value.redacted_url.host)
      || !new Set(["root", "single_segment", "nested"]).has(value.redacted_url.path_class)
      || typeof value.redacted_url.query_present !== "boolean") {
      throw new Error("invalid_response");
    }
    return value;
  };

  const captureStatusResponse = (value, jobId) => {
    if (!exactKeys(value, ["schema_version", "job_id", "state", "reason_code", "item_id", "retryable", "approvable"])
      || value.schema_version !== 1
      || value.job_id !== jobId
      || !new Set(["queued", "parsing", "fetching", "validating", "storing", "needs_user", "failed", "stored"]).has(value.state)
      || (value.reason_code !== null && (typeof value.reason_code !== "string" || !/^[a-z_]{1,64}$/.test(value.reason_code)))
      || (value.item_id !== null && (typeof value.item_id !== "string" || !/^item_[A-Za-z0-9_-]{1,123}$/.test(value.item_id)))
      || typeof value.retryable !== "boolean"
      || typeof value.approvable !== "boolean"
      || (value.state === "stored" && value.item_id === null)) {
      throw new Error("invalid_status_response");
    }
    return value;
  };

  const setCaptureState = (state, message) => {
    const pending = state === "pending";
    captureForm.dataset.state = state;
    captureForm.setAttribute("aria-busy", pending ? "true" : "false");
    captureInput.readOnly = pending;
    captureSubmit.disabled = pending;
    captureStatus.dataset.state = state;
    captureStatus.setAttribute("role", state === "error" ? "alert" : "status");
    captureStatus.textContent = message;
  };

  const appendCaptureAction = (label, handler) => {
    captureStatus.append(" · ");
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = label;
    button.addEventListener("click", handler, { once: true });
    captureStatus.append(button);
  };

  const boundedReason = (reason) => ({
    address_not_public: "网络策略拒绝了非公网地址",
    network_policy_rejected: "网络策略拒绝了该请求",
    interactive_login_required: "目标页面需要登录",
    challenge_detected: "目标页面要求交互验证",
    needs_preview: "入库前需要人工预览",
    unsupported_content: "目标内容不是受支持的文章",
    retry_exhausted: "网络重试已用尽",
    translation_failed: "内容解析失败",
    artifact_verification_failed: "内容校验失败",
  }[reason] || "任务未完成");

  const forgetRequest = () => {
    idempotencyKey = undefined;
    keyedText = undefined;
  };

  const watchCapture = async (jobId) => {
    const messages = {
      queued: `已排队 · ${jobId}`,
      parsing: `正在解析地址 · ${jobId}`,
      fetching: `正在抓取 · ${jobId}`,
      validating: `正在校验 · ${jobId}`,
      storing: `正在入库 · ${jobId}`,
      needs_user: `需要人工确认 · ${jobId}`,
      failed: `抓取失败 · ${jobId}`,
      stored: `已存入资料库 · ${jobId}`,
    };
    while (true) {
      await new Promise((resolve) => setTimeout(resolve, 1000));
      const response = await fetch(`/memex/capture/status/${encodeURIComponent(jobId)}`, {
        method: "GET",
        credentials: "same-origin",
        cache: "no-store",
        redirect: "error",
        referrerPolicy: "no-referrer",
        headers: { Accept: "application/json" },
      });
      if (!response.ok) throw new Error("status_unavailable");
      const result = captureStatusResponse(await response.json(), jobId);
      setCaptureState(result.state, messages[result.state]);
      if (result.state === "stored") {
        captureStatus.append(" · ");
        const link = document.createElement("a");
        link.href = `/memex?detail=${encodeURIComponent(result.item_id)}`;
        link.textContent = "打开文章";
        captureStatus.append(link);
        return;
      }
      if (new Set(["needs_user", "failed"]).has(result.state)) {
        captureStatus.append(` · ${boundedReason(result.reason_code)}`);
        if (result.retryable) {
          appendCaptureAction("重试", async () => {
            setCaptureState("pending", `正在重试 · ${jobId}`);
            try {
              const retry = await fetch(`/memex/capture/retry/${encodeURIComponent(jobId)}`, {
                method: "POST",
                credentials: "same-origin",
                cache: "no-store",
                redirect: "error",
                referrerPolicy: "no-referrer",
              });
              if (!retry.ok) throw new Error("retry_rejected");
              captureStatusResponse(await retry.json(), jobId);
              await watchCapture(jobId);
            } catch (_) {
              setCaptureState("error", `重试未被接受 · ${jobId}`);
            }
          });
        }
        if (result.approvable) {
          appendCaptureAction("确认入库", async () => {
            setCaptureState("pending", `正在确认 · ${jobId}`);
            try {
              const approval = await fetch(`/memex/capture/approve/${encodeURIComponent(jobId)}`, {
                method: "POST",
                credentials: "same-origin",
                cache: "no-store",
                redirect: "error",
                referrerPolicy: "no-referrer",
              });
              if (!approval.ok) throw new Error("approval_rejected");
              captureStatusResponse(await approval.json(), jobId);
              await watchCapture(jobId);
            } catch (_) {
              setCaptureState("error", `确认未被接受 · ${jobId}`);
            }
          });
        }
        return;
      }
    }
  };

  captureInput.value = "";
  captureInput.addEventListener("input", () => {
    if (captureInput.value !== keyedText) forgetRequest();
    if (captureInput.value === "") setCaptureState("idle", "");
    else {
      const validation = captureValidation(captureInput.value);
      setCaptureState(validation ? "error" : "idle", validation || "");
    }
  });

  captureForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (submitting) return;
    const text = captureInput.value;
    const validation = captureValidation(text);
    if (validation) {
      setCaptureState("error", validation);
      return;
    }
    try {
      if (keyedText !== text || !idempotencyKey) {
        idempotencyKey = newIdempotencyKey();
        keyedText = text;
      }
      submitting = true;
      setCaptureState("pending", "正在提交…");
      const response = await fetch("/memex/capture", {
        method: "POST",
        credentials: "same-origin",
        cache: "no-store",
        redirect: "error",
        referrerPolicy: "no-referrer",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/json",
          "Idempotency-Key": idempotencyKey,
        },
        body: JSON.stringify({
          schema_version: 1,
          command: "capture_public_url",
          text,
        }),
      });
      if (!response.ok) throw new Error("capture_rejected");
      const result = captureResponse(await response.json());
      const path = {
        root: "首页",
        single_segment: "单层路径",
        nested: "多层路径",
      }[result.redacted_url.path_class];
      captureInput.value = "";
      forgetRequest();
      setCaptureState(result.state, `已接收 · ${result.job_id} · ${result.redacted_url.host} · ${path} · ${result.redacted_url.query_present ? "含查询参数" : "无查询参数"}`);
      void watchCapture(result.job_id).catch(() => {
        setCaptureState("error", `状态暂不可用，任务仍可能继续 · ${result.job_id}`);
      });
    } catch (_) {
      setCaptureState("error", "提交未被接受，请检查内容或稍后重试。");
    } finally {
      submitting = false;
    }
  });

  globalThis.addEventListener("pagehide", () => {
    captureInput.value = "";
    forgetRequest();
  });
})();
