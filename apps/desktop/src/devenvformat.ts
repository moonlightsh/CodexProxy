// 开发环境检测页（设计 §16）的纯函数：检测报告 → 四行结果与修复提示。不依赖 DOM，可在 Node 测试中直接运行。
import type { StatusLevel } from "./format";
import type { DevEnvReport, MirrorReport, RuntimeReport } from "./types";

/** 安装包地址（仅用于修复提示），与内网文档 setup-for-company-network 保持一致。 */
export const PYTHON_DOWNLOAD_URL = "https://rdc.tiandy.com/nexus/repository/cypub/tools/python-3.13.14-amd64.exe";
export const NODE_DOWNLOAD_URL = "https://rdc.tiandy.com/nexus/repository/cypub/tools/node-v24.16.0-win-x64.zip";

/** 修复提示：一句说明，外加可选的一段可复制内容（命令、配置或下载地址）。 */
export interface DevEnvHint {
  text: string;
  code: string | null;
}

export interface DevEnvRow {
  label: string;
  /** 路径或地址（等宽显示） */
  addr: string;
  value: string;
  level: StatusLevel;
  hint: DevEnvHint | null;
}

export function devEnvRows(report: DevEnvReport): DevEnvRow[] {
  return [
    runtimeRow("Python", "python", report.python, pythonInstallHint(report.python.minVersion)),
    pipRow(report.pipMirror),
    runtimeRow("Node.js", "node", report.node, NODE_INSTALL_HINT),
    npmRow(report.npmMirror),
  ];
}

function pythonInstallHint(minVersion: string | null): DevEnvHint {
  const want = minVersion ? `Python ${minVersion}+` : "Python";
  return {
    text: `从内网下载 ${want} 安装程序，安装时勾选“Add python.exe to PATH”，然后重启终端与 AI 编程工具：`,
    code: PYTHON_DOWNLOAD_URL,
  };
}

const NODE_INSTALL_HINT: DevEnvHint = {
  text:
    "从内网下载 Node.js，解压后把 node-v24.16.0-win-x64 目录改名为 C:\\Tools\\nodejs（确保 C:\\Tools\\nodejs\\node.exe 存在），" +
    "再把该目录加入用户环境变量 Path，然后重启终端与 AI 编程工具：",
  code: NODE_DOWNLOAD_URL,
};

/**
 * 版本过低时的提示：检测只看 PATH 上第一个命中，系统 Path 又排在用户 Path 前面，
 * 装了新版本也可能仍命中旧版本，所以在安装提示之外补充处理办法。
 */
function tooOldHint(installHint: DevEnvHint): DevEnvHint {
  return {
    text:
      `${installHint.text.replace(/：$/, "")}。` +
      "安装后若仍检测到上面路径里的旧版本，请卸载旧版本，或在 Path 中把新版本目录移到旧版本之前。下载地址：",
    code: installHint.code,
  };
}

function runtimeRow(label: string, command: string, report: RuntimeReport, installHint: DevEnvHint): DevEnvRow {
  const addr = report.path ?? `PATH 中未找到 ${command}`;
  switch (report.state) {
    case "ok":
      return { label, addr, value: report.version ?? "可用", level: "ok", hint: null };
    case "tooOld":
      return {
        label,
        addr,
        value: `版本过低（${report.version ?? "?"}，需要 ${report.minVersion ?? "?"}+）`,
        level: "bad",
        hint: tooOldHint(installHint),
      };
    case "storeStub":
      return {
        label,
        addr,
        value: "Microsoft Store 占位程序",
        level: "bad",
        hint: {
          text:
            `PATH 上的 ${command} 是 Microsoft Store 的占位程序，并未真正安装。` +
            `请先按下面的地址安装；安装后如仍检测到占位程序，再在“设置 → 应用 → 高级应用设置 → 应用执行别名”中关闭 ${command}.exe：`,
          code: installHint.code,
        },
      };
    case "broken":
      return {
        label,
        addr,
        value: "无法运行",
        level: "bad",
        hint: { text: report.detail ?? `无法运行 ${command} --version`, code: null },
      };
    case "missing":
    default:
      return { label, addr, value: "未安装", level: "bad", hint: installHint };
  }
}

/** 取 URL 的主机名；无法解析时原样返回。 */
export function hostOf(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    return url;
  }
}

/** pip.ini 的推荐内容：http 源需要同时写 trusted-host。 */
export function pipIniSnippet(expected: string): string {
  const lines = ["[global]", `index-url = ${expected}`];
  if (expected.startsWith("http:")) {
    lines.push("", "[install]", `trusted-host = ${hostOf(expected)}`);
  }
  return lines.join("\n");
}

function mirrorCommon(label: string, report: MirrorReport, fallbackAddr: string): DevEnvRow | null {
  switch (report.state) {
    case "configured":
      return { label, addr: report.current ?? report.expected, value: "已启用", level: "ok", hint: null };
    case "skipped":
      return { label, addr: "—", value: "未检测", level: "warn", hint: null };
    case "failed":
      return {
        label,
        addr: report.current ?? fallbackAddr,
        value: "检测失败",
        level: "bad",
        hint: report.detail ? { text: report.detail, code: null } : null,
      };
    default:
      return null;
  }
}

function pipRow(report: MirrorReport): DevEnvRow {
  const label = "pip 镜像";
  const common = mirrorCommon(label, report, "—");
  if (common) return common;
  if (report.state === "untrusted") {
    const host = hostOf(report.current ?? report.expected);
    return {
      label,
      addr: report.current ?? report.expected,
      value: "缺少 trusted-host",
      level: "bad",
      hint: {
        text:
          `index-url 使用 http，但 trusted-host 没有包含 ${host}，pip 会忽略这个源。` +
          `若 pip.ini 已有 [install] 节，把 ${host} 追加到其中的 trusted-host（多个主机用空格或换行分隔），` +
          "不要再新增一个 [install] 节；没有 [install] 节时加入以下内容：",
        code: `[install]\ntrusted-host = ${host}`,
      },
    };
  }
  return {
    label,
    addr: report.current ?? "默认源（pypi.org）",
    value: "未启用",
    level: "bad",
    hint: {
      text:
        "用以下内容替换 pip.ini 中原有的 [global] / [install] 节（文件不存在则新建 %APPDATA%\\pip\\pip.ini），" +
        "不要重复写同名的节，然后重新检测：",
      code: pipIniSnippet(report.expected),
    },
  };
}

function npmRow(report: MirrorReport): DevEnvRow {
  const label = "npm 镜像";
  const common = mirrorCommon(label, report, "—");
  if (common) return common;
  return {
    label,
    addr: report.current ?? "—",
    value: "未启用",
    level: "bad",
    hint: {
      text: "在终端执行以下命令，然后重新检测：",
      code: `npm config set registry ${report.expected}`,
    },
  };
}

/** 本地时间 HH:MM:SS（“上次检测”用）。 */
export function formatClock(ms: number): string {
  const date = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}
