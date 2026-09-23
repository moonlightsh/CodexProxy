// 最近连接列表（设计 §8）：最多 50 条，最新在前，等宽数字展示。
import { clear, el } from "./dom";
import type { ConnectionRecord } from "./types";
import { formatHostPort, formatMs, formatOkMark, formatRouteLabel } from "./format";

export interface ConnectionsListRegion {
  root: HTMLElement;
  update: (records: readonly ConnectionRecord[]) => void;
}

export function createConnectionsListRegion(): ConnectionsListRegion {
  const list = el("ul", { class: "connections-list", "aria-label": "最近连接" });
  const empty = el("p", { class: "connections-empty" }, ["暂无连接记录"]);
  const root = el("div", { class: "connections-region" }, [list, empty]);

  const update = (records: readonly ConnectionRecord[]) => {
    clear(list);
    empty.hidden = records.length > 0;
    for (const record of records) {
      list.append(
        el("li", { class: "connection-row" }, [
          el("span", { class: "connection-host" }, [formatHostPort(record.host, record.port)]),
          el("span", { class: "connection-route" }, [formatRouteLabel(record.decision)]),
          el(
            "span",
            {
              class: `connection-ok ${record.ok ? "connection-ok-yes" : "connection-ok-no"}`,
            },
            [formatOkMark(record.ok)],
          ),
          el("span", { class: "connection-ms" }, [formatMs(record.ms)]),
        ]),
      );
    }
  };

  update([]);
  return { root, update };
}
