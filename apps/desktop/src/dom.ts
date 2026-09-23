// 小型 DOM 构建辅助：全部通过 textContent / 属性赋值写动态内容，不使用 innerHTML 拼接（任务 3.2 要求 7）。

type Attrs = Record<string, string | boolean | undefined>;

/** 创建一个元素并设置属性、子节点。属性值一律用 setAttribute / 属性赋值，不经过 innerHTML。 */
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs?: Attrs,
  children?: Array<Node | string | null | undefined>,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (attrs) {
    for (const [key, value] of Object.entries(attrs)) {
      if (value === undefined || value === false) continue;
      if (value === true) {
        node.setAttribute(key, "");
      } else {
        node.setAttribute(key, value);
      }
    }
  }
  if (children) {
    for (const child of children) {
      if (child === null || child === undefined) continue;
      node.append(typeof child === "string" ? document.createTextNode(child) : child);
    }
  }
  return node;
}

/** 清空一个容器的全部子节点。 */
export function clear(node: Element): void {
  while (node.firstChild) node.removeChild(node.firstChild);
}

/** 把容器内容替换为给定子节点（同样只用 DOM API，不经过 innerHTML）。 */
export function replaceChildren(node: Element, children: Array<Node | string>): void {
  clear(node);
  for (const child of children) {
    node.append(typeof child === "string" ? document.createTextNode(child) : child);
  }
}
