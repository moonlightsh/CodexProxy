// 单元测试用到的 Node 内置模块的最小环境声明。
//
// 之所以不加 `@types/node` 依赖：package.json 在本任务范围内只允许新增 scripts，
// 不允许新增依赖；这里按测试文件实际用到的最小子集手写声明即可，
// 类型检查只在 `npm run check`（tsc）时用到，真正运行由 `node --experimental-strip-types`
// 直接剥离类型执行，不依赖这份声明的运行时准确性。

declare module "node:assert/strict" {
  function ok(value: unknown, message?: string | Error): asserts value;
  function equal(actual: unknown, expected: unknown, message?: string | Error): void;
  function deepEqual(actual: unknown, expected: unknown, message?: string | Error): void;
  const assertStrict: {
    ok: typeof ok;
    equal: typeof equal;
    deepEqual: typeof deepEqual;
  };
  export default assertStrict;
}

declare module "node:test" {
  export function test(name: string, fn: () => void | Promise<void>): void;
}
