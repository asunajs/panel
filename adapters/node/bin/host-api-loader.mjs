// 让 `import ... from 'host-api'` 在任何位置的插件文件里都能解析到本适配器，
// 不依赖 node_modules 链接 / npm link。
//
// 关键点：hook 解析出的 URL 与本包自身的 file URL 完全一致，
// 所以插件 import 到的模块实例 == 运行器 import 到的实例（状态共享，不会出现两份注册表）。

const INDEX_URL = new URL('../src/index.js', import.meta.url).href;

export async function resolve(specifier, context, nextResolve) {
  if (specifier === 'host-api') {
    return { url: INDEX_URL, format: 'module', shortCircuit: true };
  }
  return nextResolve(specifier, context);
}
