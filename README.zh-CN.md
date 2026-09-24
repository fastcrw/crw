<p align="center">
  <img src="docs/logo-animation.gif" alt="fastCRW" width="180" />
</p>

<h1 align="center">fastCRW</h1>

<p align="center">
  使用同一个引擎完成搜索、抓取、URL 发现、整站爬取和结构化提取，
  将网页转换为干净的 <strong>Markdown</strong> 或结构化 <strong>JSON</strong>。
</p>

<p align="center">
  <a href="#安装"><strong>本地运行 ↓</strong></a> ·
  <a href="https://fastcrw.com/register?ref=gh-zh"><strong>使用托管 API</strong></a> ·
  <a href="https://docs.fastcrw.com"><strong>文档</strong></a> ·
  <a href="README.md"><strong>English</strong></a>
</p>

在 Firecrawl 的公开数据集上，fastCRW 的 truth-recall 为 **63.74%**，
Crawl4AI 为 59.95%，Firecrawl 为 56.04%；p50 延迟为 **1,914 ms**，
空闲内存约 **14 MB**。完整方法和复现命令见 [BENCHMARKS.md](BENCHMARKS.md)。

<p align="center">
  <a href="BENCHMARKS.md">
    <img src=".github/benchmarks/bench-radar.svg" alt="fastCRW 与 Crawl4AI、Firecrawl 的性能对比" width="100%">
  </a>
</p>

## 安装

```bash
curl -fsSL https://fastcrw.com/install | sh
```

安装后即可开始抓取；基础本地使用无需账号、API Key 或 setup。其他安装方式见
[安装指南](https://docs.fastcrw.com/installation/)。

## 五个核心操作

| 操作 | 结果 |
|---|---|
| **Scrape** | 将单个 URL 转换为 Markdown、HTML、链接、截图或结构化 JSON |
| **Crawl** | 在设定范围内爬取网站并收集页面 |
| **Map** | 快速发现网站中的 URL |
| **Search** | 搜索网页，并按需抓取搜索结果 |
| **Extract** | 按 JSON Schema 从一个或多个 URL 提取字段 |

完整请求和响应格式请查看 [API 参考](https://docs.fastcrw.com/#rest-api)。

## 选择使用方式

### CLI

```bash
crw https://example.com
crw search "rust async runtime"
```

### REST 与 SDK

请查看 [REST API 参考](https://docs.fastcrw.com/#rest-api)、
[Python SDK](https://docs.fastcrw.com/sdk-examples/#python) 和
[Node.js SDK](https://docs.fastcrw.com/sdk-examples/#typescript)。

### MCP / AI Agent

```bash
npx -y crw-mcp@latest install
```

该命令会为检测到的受支持 Agent 注册 CRW skill 和 MCP server。
未提供 API Key 时使用本地嵌入式引擎。各客户端的手动配置见
[MCP 客户端文档](https://docs.fastcrw.com/mcp-clients/)。

### 可选 Setup

```bash
crw setup
```

仅在连接 Cloud API Key，或添加本地浏览器渲染和网页搜索时运行 setup。
只有首次使用 `--summary` 或 `--extract` 时才会询问 LLM provider。

## 托管与自托管

托管和自托管都提供核心引擎操作，但可用能力、计费字段及部分响应封装
可能不同。不要假设只修改 base URL 就能让所有部署完全一致；请检查
[`/v1/capabilities`](https://docs.fastcrw.com/capabilities/) 和
[响应格式指南](https://docs.fastcrw.com/response-shapes/)。

部署、认证、容器及生产环境加固请查看
[自托管指南](https://docs.fastcrw.com/self-hosting/)。

## 贡献

项目需要 Rust 1.85 或更高版本：

```bash
git clone https://github.com/fastcrw/crw
cd crw
make check-fast
```

详细说明见 [CONTRIBUTING.md](CONTRIBUTING.md)。fastCRW 引擎采用
[AGPL-3.0](LICENSE)；通过网络调用托管或自托管 API 不会把该许可证应用到
你的客户端代码。

## Star History

<a href="https://www.star-history.com/?repos=us%2Fcrw&type=date&legend=top-left">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=us/crw&type=date&theme=dark&legend=top-left&sealed_token=Pe6pRWL7lqTM-St9eo-Cmpk5kYNyuyun0krw9eVZQFIrm3g_R2h46IW6wfNalPXquMsWSNCgKqiar1YVo9MGy2IZmN5Lz6rjZcjBCw6bCcRHORKORFRi9A" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=us/crw&type=date&legend=top-left&sealed_token=Pe6pRWL7lqTM-St9eo-Cmpk5kYNyuyun0krw9eVZQFIrm3g_R2h46IW6wfNalPXquMsWSNCgKqiar1YVo9MGy2IZmN5Lz6rjZcjBCw6bCcRHORKORFRi9A" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=us/crw&type=date&legend=top-left&sealed_token=Pe6pRWL7lqTM-St9eo-Cmpk5kYNyuyun0krw9eVZQFIrm3g_R2h46IW6wfNalPXquMsWSNCgKqiar1YVo9MGy2IZmN5Lz6rjZcjBCw6bCcRHORKORFRi9A" />
 </picture>
</a>

<sub>用户应自行遵守目标网站的政策。fastCRW 默认遵守 `robots.txt`。</sub>
