# Changelog

This page is generated from the root [`CHANGELOG.md`](https://github.com/fastcrw/crw/blob/main/CHANGELOG.md), which is maintained by release-please during releases.

:::note
The source of truth is the repository root changelog. Do not edit this docs page manually.
:::

All notable changes to CRW are documented here.

## [0.36.0](https://github.com/us/crw/compare/v0.35.1...v0.36.0) (2026-09-19)


### Features

* **scrape:** cache the fetch so a repeat scrape skips the network ([601d5f5](https://github.com/us/crw/commit/601d5f50382cd2f793562a822b675a0231be4f0c))


### Bug Fixes

* **crawl:** do not fail a job because robots.txt could not be read ([e8eea02](https://github.com/us/crw/commit/e8eea02a0339b52a19f8a0d04f252003f5408974))
* **crawl:** keep the last robots rule of a file that lands on the size cap ([1e41fb0](https://github.com/us/crw/commit/1e41fb081ab2ee8ddad530ad694b025854e2669e))
* **extract:** accumulate usage across legs, and report it on /v2/search ([6623dce](https://github.com/us/crw/commit/6623dce395bd72db53d9bf3f0592dfe16ee6eca0))
* **extract:** forward reasoning_effort from the structured builder ([dc161c1](https://github.com/us/crw/commit/dc161c1b4600ea175dea566a4a4c67e108faa99c))
* **extract:** stop corrupting markdown for non-HTML sources ([0ae7c40](https://github.com/us/crw/commit/0ae7c40251f042c1664d64a5c377dd306f56ffc1))
* **extract:** stop the rescue candidate from reintroducing page chrome ([8272600](https://github.com/us/crw/commit/8272600ec378ab388c14a16390bcc27bd63d153f))
* harden consolidated crawl reliability changes ([5691492](https://github.com/us/crw/commit/5691492669ec2e3a9fa7c35611b0b3a1b0efe23b))
* **mcp:** let CRW_LOCAL actually keep the run local ([abc05e2](https://github.com/us/crw/commit/abc05e297b4abf22ebe72f74f6a2cc5d7a9830b1)), closes [#557](https://github.com/us/crw/issues/557)
* preserve crawl and renderer reliability invariants ([65f385a](https://github.com/us/crw/commit/65f385a669aaa21605e0e043c6bb426f65b17d10))
* **renderer:** connect CDP over every resolved address, not just the first ([77c0c74](https://github.com/us/crw/commit/77c0c74decb96a444a9d5e416d0032cba2770273))
* **scrape:** scope the page cache per tenant ([e0a583b](https://github.com/us/crw/commit/e0a583b62a1ddb31da87983d92cfe0fa45bd3211))
* **sdk:** return the map sitemaps and keep local mode local ([76c59c0](https://github.com/us/crw/commit/76c59c08a0f6860840202addb9909eabf09cb307)), closes [#557](https://github.com/us/crw/issues/557)
* **v2:** carry llmUsage through to_v2_document ([85fe020](https://github.com/us/crw/commit/85fe020a478e7d256ced89a79bd523d43ee20e5f))

## [0.35.1](https://github.com/us/crw/compare/v0.35.0...v0.35.1) (2026-09-10)


### Bug Fixes

* **docker:** cross-compile BoringSSL for the arm64 image leg ([7d748e5](https://github.com/us/crw/commit/7d748e584aa0d76c83d2bc0230534bfbb055bea2))

## [0.35.0](https://github.com/us/crw/compare/v0.34.1...v0.35.0) (2026-09-10)


### Features

* **renderer:** impersonated-http tier with a real Chrome TLS fingerprint ([8a388ed](https://github.com/us/crw/commit/8a388ed83e0aca41370cc51d3ad1a179fc0d5218))


### Bug Fixes

* **release:** read the crate checksum from the sparse index ([ab43c40](https://github.com/us/crw/commit/ab43c40489f3edf446c8cbcd3c56b2c4a7fb9b23))

## [0.34.1](https://github.com/us/crw/compare/v0.34.0...v0.34.1) (2026-09-10)


### Bug Fixes

* **renderer:** report failures truthfully, scope breaker verdicts ([36f4a0a](https://github.com/us/crw/commit/36f4a0a7483ac78e9fd16beafcbd0ad25765618c))

## [0.34.0](https://github.com/us/crw/compare/v0.33.0...v0.34.0) (2026-09-08)


### Features

* **renderer:** align browser locale with the proxy exit country ([6155ac6](https://github.com/us/crw/commit/6155ac6e670a65f594270f7a39157df8c66644b6)), closes [#385](https://github.com/us/crw/issues/385)
* **renderer:** let camoufox wait out a JS challenge instead of reporting it ([86766d5](https://github.com/us/crw/commit/86766d5b329bdb2fa8af0f3d197dbae098ee9e6f))


### Bug Fixes

* **batch:** retain failed scrape results ([63b00af](https://github.com/us/crw/commit/63b00aff293ee86a17f104f0a237f63a89ad5e1b))
* keep internal URLs and proxy credentials out of error strings ([59f7a15](https://github.com/us/crw/commit/59f7a15ac0fc8baea7e1ec0af5708b358efb7a7a))
* **renderer:** escalate lightpanda to a tier the pool actually holds ([5c9f6cc](https://github.com/us/crw/commit/5c9f6cc1676a7162e84778861310ad78ba95ae30))
* **renderer:** give the chrome_proxy recovery arm time to finish ([595a3c5](https://github.com/us/crw/commit/595a3c53b008fc4bc2b03f38578b7a438b0fb582))
* **renderer:** keep the HTTP shell from resurrecting a refused wall ([c74bcf3](https://github.com/us/crw/commit/c74bcf365a72d77e025ddfcefd8640f822a1d5c2))
* **renderer:** poll the interstitial camoufox actually sees ([49a3eb9](https://github.com/us/crw/commit/49a3eb93e22626b86c98f4a5988c4094bc831614))
* **renderer:** report scrape outcomes truthfully ([294eb3d](https://github.com/us/crw/commit/294eb3d8a7cfb55026da493df9951b9d058a5968))
* **renderer:** stop camoufox polling out a page that already cleared ([800a30c](https://github.com/us/crw/commit/800a30c8f7c716e293dc64bc24664ddee44c93a6))
* **renderer:** stop shipping an unclearable wall as a success ([82a56f6](https://github.com/us/crw/commit/82a56f6bfa5f097bd046789a5db647eab2f8045e))
* **server:** make the retained batch failure legible on every surface ([15e9bb9](https://github.com/us/crw/commit/15e9bb9f3e26e41311071a4c632582d281d565a5))
* **v2:** report per-URL batch and crawl failures on the errors route ([c486731](https://github.com/us/crw/commit/c486731c52c509b6f9a7d356fef8c5b4d446c450))

## [0.33.0](https://github.com/us/crw/compare/v0.32.0...v0.33.0) (2026-09-02)


### Features

* enhance crawl error diagnostics and configurable headers ([93b4c11](https://github.com/us/crw/commit/93b4c117dd7dd569f1f0b9abce8d3416cf162e75))


### Bug Fixes

* **crawl:** report unreadable pages and honour per-request headers ([c58a8f6](https://github.com/us/crw/commit/c58a8f6e4d09bff7c5686b1db093f1571fa47988))
* **deps:** move PDF parsing to lopdf 0.42, clearing RUSTSEC-2026-0187 ([10f4341](https://github.com/us/crw/commit/10f4341eaa9bb957e5b2bcfb7424fdb131e844b1))
* **extract:** enforce structured output on the chat transports ([aeb55ca](https://github.com/us/crw/commit/aeb55ca844b32197859252679d9a28290aab31d7))
* **renderer:** don't feed binary bodies to the HTML extractor ([ab5e93e](https://github.com/us/crw/commit/ab5e93e6d116ed1aa5a17c5a4193122fb18b035e))
* **renderer:** keep the binary-body check off real pages ([4f741d5](https://github.com/us/crw/commit/4f741d57d03170617b30e6dbbc4e83e7af6afbff))
* **scrape:** classify a wall and an empty result as failures ([96a4b43](https://github.com/us/crw/commit/96a4b4395366cafcd1a6314b7a0af8fb6db82cab))

## [0.32.0](https://github.com/us/crw/compare/v0.31.0...v0.32.0) (2026-08-24)


### Features

* **map:** return discovered sitemap document URLs ([c65ea1c](https://github.com/us/crw/commit/c65ea1c607609b3f6c2e7874f2789c307fb56fa6)), closes [#440](https://github.com/us/crw/issues/440)
* **setup:** register MCP with detected agents on scripted setup ([2cb5f71](https://github.com/us/crw/commit/2cb5f711f48f8bbf04def1e6a62dc44ed3939272))


### Bug Fixes

* **browse:** satisfy the lints Rust 1.98 turned on ([a338091](https://github.com/us/crw/commit/a338091983cf697d84bf2e51d251c7538c45832b))
* **map:** report a sitemap by retrieval, not by entry count ([38b1c1f](https://github.com/us/crw/commit/38b1c1ffd1e4e881e25ea20ffebf17bfc90fa43f)), closes [#440](https://github.com/us/crw/issues/440)
* **mcp:** bound the map sitemaps list like the links list ([579910a](https://github.com/us/crw/commit/579910a5259a81cc80ad199fddeeed3e43fd54be)), closes [#440](https://github.com/us/crw/issues/440)
* **mcp:** pair the Smithery API key with the cloud API URL ([566eb85](https://github.com/us/crw/commit/566eb85eaa1d1fa6b3a2fda491f49816ea244a23))
* **renderer:** stop treating a cleared page as a Cloudflare challenge ([0b91afb](https://github.com/us/crw/commit/0b91afb6f19ed0458cd09a7edcc3e19d1c38b453))
* **research:** restore arXiv coverage and unbreak the OpenAlex pool ([a942b6d](https://github.com/us/crw/commit/a942b6d303dec0e2461062cab2c74dedf0ba0ec8))
* **sdk:** follow batch pagination and stop on cancel in both SDKs ([47dd99b](https://github.com/us/crw/commit/47dd99b9ee4b56b13c86d44173af7b1a3ed70893))

## [0.31.0](https://github.com/us/crw/compare/v0.30.0...v0.31.0) (2026-08-18)


### Features

* **search:** carry a paid-rescue entitlement to the search backend ([d7812a0](https://github.com/us/crw/commit/d7812a0e62fd4f5d92660df3254ce01146f78d8e))
* **search:** per-request query-expand override and deadline-bounded scout ([8436b43](https://github.com/us/crw/commit/8436b434ca3b5ebc615bc81337ca7ec27e2c5118))
* **search:** snippet-first lazy-scrape answer path (gated, default off) ([32382c4](https://github.com/us/crw/commit/32382c46f5bcb015b2d4a3c38a241a75ae33064b))
* **setup:** offer MCP registration for detected AI tools ([8d78c0c](https://github.com/us/crw/commit/8d78c0cf937c0988e4de189ec5ed5198bb7b95a5))
* simplify onboarding and align documentation truth ([9748877](https://github.com/us/crw/commit/974887780f0653ec8a96f450eb79843088533d8d))


### Bug Fixes

* **cli:** drop emoji from setup output ([a781671](https://github.com/us/crw/commit/a781671b3ad7ff4125b2689d1ac3422906319245))
* **cli:** keep the smoke fixture inside the published crate ([b3f85c6](https://github.com/us/crw/commit/b3f85c61d1539e79deba09cc8643fb6b44131d46))
* **config:** pin the deployed config shape, and sync the embedded spec ([16ea2ef](https://github.com/us/crw/commit/16ea2efe188e3dabe3e24ccb5b28e9fb50983b2f))
* **crawl:** apply the block verdict on crawl, batch and extract ([def3f9c](https://github.com/us/crw/commit/def3f9c7dd2a1334d1cb26b5cca605f59a3449ea))
* **deps:** hold pdf-inspector at 0.1.6, 0.1.7 loses page content ([aef4719](https://github.com/us/crw/commit/aef471913b82e526a758263639b461f11a2943f7))
* **extract:** retry an LLM POST that died before reaching the provider ([a9da7db](https://github.com/us/crw/commit/a9da7db01e78b76795b3f69073eec19577205447))
* **extract:** stop calling valid non-HTML content an anti-bot block ([04c5f63](https://github.com/us/crw/commit/04c5f6370dc864e10dfd56473fed833d545ef1d3))
* **install:** resolve latest via GitHub redirect, not the REST API ([cd78a60](https://github.com/us/crw/commit/cd78a6099bbc64da2faedbece82a453b81b0ff85))
* **renderer:** report a blackholed origin as unreachable, not a timeout ([6250190](https://github.com/us/crw/commit/625019081de6e086e520774bba7b4541aaca2d54))
* **renderer:** search for &lt;/body&gt; after &lt;body&gt;, not from the start ([28564bb](https://github.com/us/crw/commit/28564bbb129b526bc931b26149877ad0094d3f46))
* **scrape:** stop returning an origin error page as a successful scrape ([13106ae](https://github.com/us/crw/commit/13106aed62def301ad4ed853827a2bf598a70d86))
* **scrape:** stop selling registrar parking pages as content ([68f4545](https://github.com/us/crw/commit/68f45456231bfc6895db81131b59a92ce1f14b6d))

## [0.30.0](https://github.com/us/crw/compare/v0.29.0...v0.30.0) (2026-08-07)


### Features

* **mcp:** add option to hide credit-billing fields ([bc951f1](https://github.com/us/crw/commit/bc951f1ce687657341c8844e173b7664632c65a5))
* **renderer:** make the interactive render reserve configurable ([9fdcde8](https://github.com/us/crw/commit/9fdcde8d5ee071cc75e933b2dfdb92c7c4172508))


### Bug Fixes

* **extract:** drop single-render nav menus under onlyMainContent ([c5bd7e6](https://github.com/us/crw/commit/c5bd7e6f27ec56abbc82eb1394cc0fef03964d34))
* **mcp:** scope credit-field strip and honor config in proxy mode ([b8899d6](https://github.com/us/crw/commit/b8899d66b2021ae9d14e79bca3e5634c1756610d))
* **renderer:** attribute nav-budget log to the real renderer tier ([a8af83a](https://github.com/us/crw/commit/a8af83a4539a0e12fd797563c13b0d0344a1af41))
* **renderer:** classify a blackholed origin with a hung proxy rescue as unreachable ([329c478](https://github.com/us/crw/commit/329c478ccc2e942159204ab9516106430f3909ba))
* **renderer:** close the guard's remaining fail-open and budget holes ([1f5edeb](https://github.com/us/crw/commit/1f5edeba3e35427cb27abc8e23647b4c2bf0c477))
* **renderer:** keep the outbound guard closed under teardown and pressure ([4695bfa](https://github.com/us/crw/commit/4695bfaa6a1f3f739d2f6888c1a376db77c06c22))
* **renderer:** reap cancelled legacy renders and keep denials attributable ([27807f6](https://github.com/us/crw/commit/27807f672361639fea2a4707c7da8c91ba715bd7))
* **renderer:** stop releasing unjudged requests at render teardown ([d57f6a4](https://github.com/us/crw/commit/d57f6a411cff2c6c009a54b43b2daef79583e437))
* **renderer:** validate outbound destinations in the CDP render tiers ([30ca299](https://github.com/us/crw/commit/30ca299d15b65b352e67edf90ccf28eb33f6cc02))


### Performance

* **crawl:** stop rendering JSON bodies that score low on quality ([a973993](https://github.com/us/crw/commit/a973993dd91ede70c658f5fd92ff0183a9577012))
* **renderer:** cut chrome challenge retries 3 to 1 in docker config ([81774c7](https://github.com/us/crw/commit/81774c767e09ab1be05c2ff4aff3d29d0e638531))

## [0.29.0](https://github.com/us/crw/compare/v0.28.0...v0.29.0) (2026-08-04)


### Features

* **extract:** expand table rowspan/colspan into a flat grid ([0f486aa](https://github.com/us/crw/commit/0f486aa556fda6e24f2b683e9f593ae39f8013d4))
* **extract:** support openai-responses LLM provider ([ed551e3](https://github.com/us/crw/commit/ed551e3257da61a238efc188c9c064462591851d))
* **renderer:** cf_clearance cookie cache — solve a Cloudflare challenge once, reuse across the host ([c9b6e6d](https://github.com/us/crw/commit/c9b6e6d5cafb361cbfc637dc34c3e1acd1cef96b))
* **renderer:** decouple chrome_proxy arm pool from renderer.pool_size ([3b0ef22](https://github.com/us/crw/commit/3b0ef229802677fe29a74f1b06b4582be323f7c7))
* **renderer:** fire the cloak CF-recovery arm on a fresh budget (gated) ([9f1e5ea](https://github.com/us/crw/commit/9f1e5ea5555bbf29cd41e454902be0cd804a15e4))


### Bug Fixes

* **antibot:** catch Reddit/Cloudflare/Vercel block pages sent as 200 ([e7b2f5b](https://github.com/us/crw/commit/e7b2f5b988aec7188592ad4d128e3f8f1115d556))
* **antibot:** catch Vercel block pages that beat the markdown guard ([0d95dad](https://github.com/us/crw/commit/0d95dadc91c1583f8ef482767854bcea9932f792))
* **core:** stop SSRF guard over-blocking public /16s ([555d62b](https://github.com/us/crw/commit/555d62bc2e55fd0ccb870ab4b5f6732281393a1d))
* **extract:** drop two over-broad boilerplate patterns ([ee5993c](https://github.com/us/crw/commit/ee5993c9d03705f165f05a849346ad416662c25f))
* **extract:** keep Elementor page content and collapse repeated nav ([86c1d2d](https://github.com/us/crw/commit/86c1d2d1ab4c92e3b85c6a6530c92aa4f5ad2df8))
* **extract:** stop deleting article content under onlyMainContent ([682a731](https://github.com/us/crw/commit/682a731c56bf31da1b14f716ae26a227de8c6285))
* **extract:** validate Responses status and stop echoing error bodies ([9cd06f4](https://github.com/us/crw/commit/9cd06f43bd77fc48863c51e6906e8d3d1bd76657))
* **mcp:** stop advertising an output schema the proxy cannot honour ([e76bcbc](https://github.com/us/crw/commit/e76bcbcdbb37291cedadd122a59d104c0c7683f8)), closes [#391](https://github.com/us/crw/issues/391)
* **renderer:** cloak solves cold hosts on a fresh exit IP, not a pinned one ([c2403f2](https://github.com/us/crw/commit/c2403f25013598ea5be162a66605649b288fde5e))
* **renderer:** pool the chrome_proxy tier so its targets get reaped ([1bf32e8](https://github.com/us/crw/commit/1bf32e8caf058bde8016e17af11d0458472ee21e))
* **renderer:** reap orphaned chrome targets on cancelled renders ([727a041](https://github.com/us/crw/commit/727a0419a7f91bf0de1414d0324565b40917fd79))
* **renderer:** stop anti-bot blocks from stranding the recovery tier ([223de6c](https://github.com/us/crw/commit/223de6c4c32ff4ba1bd1f13fdf9ed310a3a6f540))
* **renderer:** stop discarding good content when the JS ladder fails ([6d36665](https://github.com/us/crw/commit/6d366651de0801525857e95e16569d242daf3e68))
* **scrape:** stop reporting a thin page as an anti-bot block ([544d4aa](https://github.com/us/crw/commit/544d4aa1a76bff2789c1d368d15c665e630bdbd9))
* **scrape:** stop selling a CDN's error page as the page that was asked for ([d9ddca9](https://github.com/us/crw/commit/d9ddca9e9a22eb1d108e1f34bbd182543f67c54f))
* **search:** validate `lang` at the API boundary ([52711fe](https://github.com/us/crw/commit/52711fe42b1acbdff54b18b57f1e44653c2aad7c))

## [0.28.0](https://github.com/us/crw/compare/v0.27.1...v0.28.0) (2026-07-23)


### Features

* **sdk:** add typed response object interfaces ([a2acacb](https://github.com/us/crw/commit/a2acacb1e6cbf3077fdd089f710cac75eb7eabda)), closes [#361](https://github.com/us/crw/issues/361)


### Bug Fixes

* **core:** bound SSRF DNS resolution with an 8s timeout ([4b542dc](https://github.com/us/crw/commit/4b542dc65dd01465cc0e917f4607dc9095d00e18))
* **crawl:** accept the nested scrapeOptions shape the spec publishes ([c568333](https://github.com/us/crw/commit/c5683334414df28525c7efb42332fb2e2165858f))
* **openapi:** re-sync the docs copy of the 3.0 spec ([ffe8618](https://github.com/us/crw/commit/ffe8618b24cece724ecc62d2f8d044d37d3816a4))
* **openapi:** sync the spec with the real types, and guard it in CI ([dcc0fcc](https://github.com/us/crw/commit/dcc0fcc77d8b926fe4744564072d59b81f4dcef6))
* **renderer:** forward custom request headers on the CDP path ([a0677f7](https://github.com/us/crw/commit/a0677f7a75d58b5330def3ec88527f60d50a9351)), closes [#351](https://github.com/us/crw/issues/351)
* **renderer:** stop misclassifying &gt;500KB pages as thin ([21c15dd](https://github.com/us/crw/commit/21c15dd7910afcd3b228d6f3c11e3dda9005b7b0))
* **renderer:** treat a blank caller UA as absent, document header scope ([69a2f8e](https://github.com/us/crw/commit/69a2f8e0f44f249a80f4fde3cec11232b6cae976))
* **search:** bound the per-result scrape budget for scrapeOptions ([97aa154](https://github.com/us/crw/commit/97aa15431c06b80bf2dbfaec37c381ec09b4e281))
* **v2:** honor location.languages as an Accept-Language header ([bac52cd](https://github.com/us/crw/commit/bac52cdf2a2fd505cc60322ef8140a2ec40d6d16))
* **v2:** honor renderJs on the Firecrawl-compat surface ([8491d5f](https://github.com/us/crw/commit/8491d5f4d2903b2d01999d457db4b32732aacc37)), closes [#346](https://github.com/us/crw/issues/346)
* **v2:** thread the extraction prompt through extract and the json format ([0f2650e](https://github.com/us/crw/commit/0f2650e45f96b715986b7fbc861ba817bcad2c54)), closes [#352](https://github.com/us/crw/issues/352)


### Performance

* **search:** parallelize enrichment DNS validation and dedup scout scrapes ([6e1ca78](https://github.com/us/crw/commit/6e1ca78ef3e8b79994e98e88b9d7f287d7ed204e))

## [0.27.1](https://github.com/us/crw/compare/v0.27.0...v0.27.1) (2026-07-22)


### Bug Fixes

* **packaging:** verify downloaded binaries against published checksums ([a181e69](https://github.com/us/crw/commit/a181e6906ce3427258ab34703c64c47be0957883))
* **search:** distinguish a degraded backend from a zero-result query ([c8c031a](https://github.com/us/crw/commit/c8c031a13793d96bbd05d4ebabd1846c5f88117a))

## [0.27.0](https://github.com/us/crw/compare/v0.26.1...v0.27.0) (2026-07-21)


### Features

* **docker:** make published bind address configurable ([79ac8f7](https://github.com/us/crw/commit/79ac8f700895694a05d7c2f097617930e0784892))
* **renderer:** route learned CF-managed domains straight to cloak ([884ed63](https://github.com/us/crw/commit/884ed635b19af2dd3b80092deb83072ff2f1982a))


### Bug Fixes

* **compose:** make crw host port overridable via CRW_HOST_PORT ([3f0a1eb](https://github.com/us/crw/commit/3f0a1ebbdc7d0e749c29460c99a6953b171932d3))
* **crawl:** stop flagging cleared managed pages as CF challenges ([9d81efe](https://github.com/us/crw/commit/9d81efebd33d7ad34f0c1b64f73f6bc64bbc8f7b))

## [0.26.1](https://github.com/us/crw/compare/v0.26.0...v0.26.1) (2026-07-21)


### Bug Fixes

* **extract:** count MAX_USER_PROMPT_CHARS as Unicode scalars ([ea798f5](https://github.com/us/crw/commit/ea798f5abf9df7b4448cb1d5732b8ac5511d37b6))
* **extract:** count maxChars as Unicode scalars ([f319de6](https://github.com/us/crw/commit/f319de682a274e2b2c62f514fe19834deb581cad))
* **server:** require auth on admin and metrics routes, restrict CORS ([1a66247](https://github.com/us/crw/commit/1a662473da9a98173c4f7e6d42280049799d3895))

## [0.26.0](https://github.com/us/crw/compare/v0.25.2...v0.26.0) (2026-07-19)


### Features

* **extract:** expose extracted images in scrape response ([7d632ef](https://github.com/us/crw/commit/7d632ef56cd20fd9a30784bb400b19593cdad74e))
* **renderer:** let the cloak arm self-provision its residential proxy ([35bc472](https://github.com/us/crw/commit/35bc4728913628f310f0872573c3bb7b5b93b87f))
* **renderer:** send x-deadline-ms to the cloak sidecar mirror call ([0de10e2](https://github.com/us/crw/commit/0de10e276a4e04ffa595ff0adc06e146bedc725c))
* **search:** passage-select + snippet-first for the answer path ([88f2e93](https://github.com/us/crw/commit/88f2e93bc43469990f3fc197f2652f7e74565f49))
* **server:** add Kimi Code compat routes (/kimi/search, /kimi/fetch) ([5a241e5](https://github.com/us/crw/commit/5a241e5f2fd810d18d7fa077f3b3e036a92ec5fd))


### Bug Fixes

* **renderer:** give chrome_proxy recovery arm its own budget and load-shed it ([1a3942b](https://github.com/us/crw/commit/1a3942b02efe659377e8dcde6986b9b2ea09db6f))
* **server:** add success field to /v1/extract responses ([4f386ed](https://github.com/us/crw/commit/4f386ed2acd404b2b7b5b540bcb2079dab4451be)), closes [#318](https://github.com/us/crw/issues/318)

## [0.25.2](https://github.com/us/crw/compare/v0.25.1...v0.25.2) (2026-07-17)


### Bug Fixes

* **release:** stop the mcp-registry verify failing on a published version ([24cebb0](https://github.com/us/crw/commit/24cebb001cd23e1b7b0a31024795a75d82349de0))
* **renderer:** fire chrome_proxy recovery arm under the default deadline ([e6c19a1](https://github.com/us/crw/commit/e6c19a1fc71b1ee422f8c6391737f3503f8b684f))
* **renderer:** suppress chrome_proxy on antibot-detected fingerprint walls ([ff09f30](https://github.com/us/crw/commit/ff09f3004c76290ed9cd9b376ed774ed20ae626b))
* surface and recover HTTP-200 Wikimedia datacenter block shell ([a148f60](https://github.com/us/crw/commit/a148f609fd15e430481352a794eaacde6851bd8c))

## [0.25.1](https://github.com/us/crw/compare/v0.25.0...v0.25.1) (2026-07-16)


### Bug Fixes

* **browse:** stop a rejected multibyte url from panicking the log path ([66a394d](https://github.com/us/crw/commit/66a394d139e8d6d14371953fef4cc04e2d919b61))
* **extract:** stop a multibyte value from panicking the basis scan ([5efddc4](https://github.com/us/crw/commit/5efddc42f3f8ed48a311c4f6047a4c32122dab3c))
* **renderer:** avoid panic slicing multibyte HTML at the scan-size cap ([95fe189](https://github.com/us/crw/commit/95fe1894e6d98b6e32ef58fd9110020162e2655b))

## [0.25.0](https://github.com/us/crw/compare/v0.24.1...v0.25.0) (2026-07-16)


### Features

* **engine:** add opt-in cloak Turnstile-solver recovery tier ([41f780e](https://github.com/us/crw/commit/41f780e0ea5d4e74d363a79cd108b33663b00bb1))
* **extract:** add canonical cancellation lifecycle ([635679f](https://github.com/us/crw/commit/635679f3c279d1961db1ad021c3eb2bd800a34c4))


### Bug Fixes

* **engine:** detect large Cloudflare challenge pages and return clean blocks ([b3e8158](https://github.com/us/crw/commit/b3e81585594c030a922df72af1a811d4fb8cc0d5))
* **extract:** carry selector_no_match through the full pipeline for include_tags ([819372d](https://github.com/us/crw/commit/819372deb66071efbf37d6b08e5b119f6f78f7a7))
* **extract:** don't relabel a finished job as cancelled ([cb05be2](https://github.com/us/crw/commit/cb05be2d590f7310887cb40485299c2d037cd911))
* **extract:** enforce lifecycle expiry invariants ([903fbb9](https://github.com/us/crw/commit/903fbb9475d6c7680690653db825df0903e5c830))
* **extract:** return empty + selector_no_match when a selector matches nothing ([7a1ac76](https://github.com/us/crw/commit/7a1ac760d30213fbcbf74cb2d8694ffc4c9fc34f))
* **map:** bound the seed SSRF check and widen the latched direct-rescue ([b55e081](https://github.com/us/crw/commit/b55e0817c9c9cd6628d404a0fbd7572759393834))
* **map:** stop returning 504 with zero URLs on sites without a sitemap ([159b0a3](https://github.com/us/crw/commit/159b0a34f5a273fe7ef38dd5e78cc11307b5cb46))
* **mcp:** keep the success envelope on extract lifecycle tools ([0bff321](https://github.com/us/crw/commit/0bff32141e1711a969b6bba6f51da12fbba43636))

## [0.24.1](https://github.com/us/crw/compare/v0.24.0...v0.24.1) (2026-07-14)


### Bug Fixes

* **engine:** drop hickory-dns so direct egress resolves in Docker ([4cf098f](https://github.com/us/crw/commit/4cf098f471307e199c09575450c42c9f88ece87e))
* **renderer:** detect Chrome on Windows in embedded mode ([7d3bf0f](https://github.com/us/crw/commit/7d3bf0f032be394e0fc3fc63706b7f1e567c3296))

## [0.24.0](https://github.com/us/crw/compare/v0.23.0...v0.24.0) (2026-07-12)


### ⚠ BREAKING CHANGES

* **server:** the `monitor` cargo feature and the crw-monitor crate are removed. Self-hosted scheduled monitoring must be driven by an external scheduler calling the stateless change-tracking primitives.

### Features

* **bench:** live answer track + managed-endpoint support ([278d6d5](https://github.com/us/crw/commit/278d6d52fb694e6820f27014413fb9f9dfd9f742))
* **bench:** multi-track benchmark discipline ([887623c](https://github.com/us/crw/commit/887623c885c9a7299d34a622254f78a5882a3cb6))
* **extract:** per-field evidence contract (basis) ([273f2da](https://github.com/us/crw/commit/273f2da9c5ddf620264c2976b565fd7b7007097c))
* **mcp:** advertise server instructions and web-first tool guidance ([23cc08d](https://github.com/us/crw/commit/23cc08dfeb88dd9f732bf8e0580ba2138dc52e47))
* **server:** derive /v1/capabilities from the real build and config ([cd5818c](https://github.com/us/crw/commit/cd5818c63c19e4de9bada7b5ebdade4eda871fb2))
* **server:** native /v1/batch/scrape endpoint ([3a51f33](https://github.com/us/crw/commit/3a51f3364b47e744cbb040c4e4b823e3870cd5ae))


### Bug Fixes

* **engine:** accumulate LLM usage across every leg, not just the first ([c83de80](https://github.com/us/crw/commit/c83de80c0c368cfa7b9437c9b6e25faf840fd3a1))
* **mcp:** align instructions with advertised tools, scrub backend name ([392e390](https://github.com/us/crw/commit/392e3904e3aaea5c561e958eb15e96ad1980a480))
* **renderer:** keep chrome_proxy in the screenshot ladder for captures ([4152404](https://github.com/us/crw/commit/41524042081fe33d92fb96e45ee9f2c6a2626c29))
* **renderer:** recover blocked-egress scrapes and stop degenerate JS attempts ([376f8d9](https://github.com/us/crw/commit/376f8d98abfadee947c2df3b1320e418276e3978))
* **renderer:** recover connect-timeout blackholes via the fallback proxy ([1e14879](https://github.com/us/crw/commit/1e14879eb2325ad9d96d1231eaa656f3a3b68bc3))
* **sdk:** extract works against the managed API, and search stops discarding the answer ([612df13](https://github.com/us/crw/commit/612df13d3b8dfb1c37fe59046122733d04caef59))
* **server:** stop advertising extract and baseUrl the instance cannot honour ([db358b0](https://github.com/us/crw/commit/db358b09ba878abfb2e8c08fabb6a8ca2da5be37))
* stop leaking the search backend's name to users, and guard it ([22098d6](https://github.com/us/crw/commit/22098d6d464f8416dd09b576fd58d87a7cf4e4fd))


### Code Refactoring

* **server:** remove the stateful monitor layer from open core ([bf6d221](https://github.com/us/crw/commit/bf6d221ed32111bda21e4ae184a42ff6da9b2143))

## [0.23.0](https://github.com/us/crw/compare/v0.22.0...v0.23.0) (2026-07-09)


### ⚠ BREAKING CHANGES

* **server:** SDK extract() now returns a per-URL results array instead of a merged object, and no longer accepts systemPrompt or baseUrl.

### Features

* **server:** native /v1/extract endpoint ([b5fc494](https://github.com/us/crw/commit/b5fc49441a440da37f9a094fb68a9a813380a814))
* **server:** reserve interactive capacity from batch + plan-scaled width ([ada487d](https://github.com/us/crw/commit/ada487de8b8b7b98ece7eed012da0d406e2d21de))


### Bug Fixes

* **docker:** CARGO_BUILD_JOBS via env, not empty -j flag ([37282d2](https://github.com/us/crw/commit/37282d2bbd2fd38d50c8f0cfc1977bcb5fa19ff9))


### Performance

* **docker:** build only requested binaries via CARGO_PKGS arg ([98b6626](https://github.com/us/crw/commit/98b6626bc949cc1b89401306b5bb31ab47e60b89))

## [0.22.0](https://github.com/us/crw/compare/v0.21.3...v0.22.0) (2026-07-09)


### Features

* **cli:** non-interactive cloud setup via 'crw setup --api-key' ([e7cc4f9](https://github.com/us/crw/commit/e7cc4f99b29d88608b1fbe3359fab8ba179695ce))
* **installer:** connect to cloud when CRW_API_KEY is set ([fedb5de](https://github.com/us/crw/commit/fedb5de046da3578d2207ba40bf66671d198f9c9))
* **server:** batch scrape scale guards for 10k-URL submits ([9a3e0b6](https://github.com/us/crw/commit/9a3e0b64b0a0b6663315912c205f4278b8419801))


### Bug Fixes

* **clippy:** satisfy clippy 1.97 lints (for_kv_map, manual_filter) ([ed4522d](https://github.com/us/crw/commit/ed4522d943770999717531fe07cf75b17611697d))
* **mcp:** launch server via 'npx -y crw-mcp' so fresh installs work ([a574a61](https://github.com/us/crw/commit/a574a61d587cc6e287a7d9d38f63ce42ada3e558))
* **server:** cancelled crawl/batch jobs reach a terminal state ([fee388c](https://github.com/us/crw/commit/fee388cf3cc5570c5980ae60d73fe5a85f658d36))


### Performance

* **docker:** cargo-chef dependency layer for faster engine builds ([603acfa](https://github.com/us/crw/commit/603acfa94850ca3face85295d6091e11a7b6ade2))
* **server:** raise batch URL validation concurrency to 256 ([651d3c0](https://github.com/us/crw/commit/651d3c0a6a3d6451492aa725e7a387669f3d7103))

## [0.21.3](https://github.com/us/crw/compare/v0.21.2...v0.21.3) (2026-07-08)


### Bug Fixes

* **ci:** let release doc-sync pushes bypass branch protection ([7510d1e](https://github.com/us/crw/commit/7510d1ec6d28e431ab734a040aa423a756c842f8))

## [0.21.2](https://github.com/us/crw/compare/v0.21.1...v0.21.2) (2026-07-08)


### Bug Fixes

* **renderer:** load images/media/fonts for screenshots ([2310e5c](https://github.com/us/crw/commit/2310e5cf0b18447c77dd37807b7a28dc78cb9dfc))
* **renderer:** resolve CDP ws host to IP for Chromium 148+ rebinding guard ([b197b95](https://github.com/us/crw/commit/b197b950140b772b313137868fd8dc627e834c37))

## [0.21.1](https://github.com/us/crw/compare/v0.21.0...v0.21.1) (2026-07-07)


### Bug Fixes

* **scrape:** detect modern Cloudflare Turnstile 200 interstitial ([ce3771f](https://github.com/us/crw/commit/ce3771fbbe5f7fd3da1ea90761594ba3a2032633)), closes [#350](https://github.com/us/crw/issues/350)
* **scrape:** scan full html for CF challenge markers, not an 80KB prefix ([99fe2b2](https://github.com/us/crw/commit/99fe2b2d6dd3d998d9e3a3d8c30f91b032b43671)), closes [#350](https://github.com/us/crw/issues/350)

## [0.21.0](https://github.com/us/crw/compare/v0.20.0...v0.21.0) (2026-07-06)


### ⚠ BREAKING CHANGES

* **api:** error responses now use `errorCode` instead of `error_code`. The managed API already strips the field, so managed clients are unaffected; self-hosted clients parsing `error_code` should read `errorCode`.

### Features

* **cli:** add crw bench FRAMES harness ([fb78bcb](https://github.com/us/crw/commit/fb78bcb57b3add83db28e5ecdbaf39ef39d067fe))
* **cli:** bench A/B flags, concurrency, crash-safe writes ([7eb059e](https://github.com/us/crw/commit/7eb059edac5ea1f857aca34e97a83145928064ee))
* **core:** add evidence & provenance primitives ([4a638a0](https://github.com/us/crw/commit/4a638a08e5e1ee3502854e20ae01afd885aa2353))
* **core:** expose sourceHash on scrape responses ([b0d89f9](https://github.com/us/crw/commit/b0d89f925d98a36bb7ad3cff9b28a7ec1f5a238f))
* **extract:** prompt-based extraction and full meta-tag metadata ([6453204](https://github.com/us/crw/commit/645320407c254768f91e75f391350035a11342ba))
* **proxy:** retry with default country on residential CONNECT tunnel failure ([8aafdc8](https://github.com/us/crw/commit/8aafdc8a598d3e3aca0ef714749382a926f8a32a))
* **search:** thread per-request country to result-page scraping ([be3b0d1](https://github.com/us/crw/commit/be3b0d1797d7559adc4e459b89aa23bf1c4f3832))
* **server:** add /firecrawl/* compat namespace, own /v1 as native API ([d9cb3b8](https://github.com/us/crw/commit/d9cb3b87a8ad36c75788793b236a822596370875))


### Bug Fixes

* **api:** rename error_code response field to errorCode ([e4df682](https://github.com/us/crw/commit/e4df682b863821ab4c71752881ca7304e5a718ad))
* **cli:** repair clap arg conflict that panicked debug builds ([f2a6b77](https://github.com/us/crw/commit/f2a6b7798fcdb6bd1c1badc420ad8bd517b6ae67))
* **docs:** add redirect stubs for pre-flatten legacy doc URLs ([a1f29dc](https://github.com/us/crw/commit/a1f29dc9b3f4573bef96a7a4724846477aa7ab6c))
* **extract:** unify untrusted-content fencing; nonce-fence the change judge ([192b9d7](https://github.com/us/crw/commit/192b9d742cb4455f70370c737afd29761189018a))
* **renderer:** cap full-page screenshot height to avoid OOM ([c5f555d](https://github.com/us/crw/commit/c5f555de54b41a088e2be66fa7c2be6333c7ac5d)), closes [#161](https://github.com/us/crw/issues/161)
* **renderer:** don't browser-render thin pages that ship no JS ([f42f14a](https://github.com/us/crw/commit/f42f14acfdde646ec2223832b138f3cf2952531c))
* **scrape:** report anti-bot/challenge pages as blocked, not success ([d06f051](https://github.com/us/crw/commit/d06f05189f8bec911c63dada4c27d1b5daf8e478))
* **search:** charset-aware scrape decode + per-result error + neutral answer warnings ([a9870e2](https://github.com/us/crw/commit/a9870e2faa25a23de0d004054c37a7f04f026fa8))

## [0.20.0](https://github.com/us/crw/compare/v0.19.0...v0.20.0) (2026-07-01)


### Features

* **map:** escalate anti-bot-gated sitemaps through the JS renderer ([2634a7e](https://github.com/us/crw/commit/2634a7e0a5c2670d8075ef1ef5547716e62ad2e3))
* **map:** fix URL discovery on hard sites + configurable discovery limit ([f5fe1f4](https://github.com/us/crw/commit/f5fe1f4a9487643e7e58ef7d5c29e5d988fcdc42))


### Bug Fixes

* **renderer:** flat 1-credit cost for every renderer ([35a604c](https://github.com/us/crw/commit/35a604cabf375d5e956adc0ec18f017e57309e79))

## [0.19.0](https://github.com/us/crw/compare/v0.18.3...v0.19.0) (2026-06-28)


### Features

* **renderer:** proxy-retry on origin rate-limit (429) ([da7ecda](https://github.com/us/crw/commit/da7ecdaff97b6d9aa995f4353afeadae91b328ef))
* **renderer:** relaxed-TLS fallback for cert-broken origins ([13d39f6](https://github.com/us/crw/commit/13d39f692d42cc64f00389bf55943dd51a6a49ad))


### Performance

* **engine:** offload HTML extraction off the async reactor ([1683153](https://github.com/us/crw/commit/16831532dff80047fd9f9039aafc43b284e8dbca))

## [0.18.3](https://github.com/us/crw/compare/v0.18.2...v0.18.3) (2026-06-23)


### Bug Fixes

* **renderer:** strip "Mozilla" UA prefix for lightpanda (it rejects it) ([fda6139](https://github.com/us/crw/commit/fda6139a74c3c57aa38665d93e5123090bc71a32))

## [0.18.2](https://github.com/us/crw/compare/v0.18.1...v0.18.2) (2026-06-23)


### Bug Fixes

* **renderer:** send a modern UA on the CDP path (setUserAgentOverride) ([ffcf564](https://github.com/us/crw/commit/ffcf5640384d49bf5ebe2c72e8ba91001bf2e931))

## [0.18.1](https://github.com/us/crw/compare/v0.18.0...v0.18.1) (2026-06-23)


### Bug Fixes

* **npm:** ship bin/install.js + bin/agents.js in the crw-mcp package ([437dbc7](https://github.com/us/crw/commit/437dbc760630e58819cfcb860d84dfb3ba5061b5))

## [0.18.0](https://github.com/us/crw/compare/v0.17.0...v0.18.0) (2026-06-23)


### Features

* **extract:** optional reasoning_effort config field ([e677190](https://github.com/us/crw/commit/e677190a6c54d0987e09ac9dcc5580f5df28371a))
* **install:** auto-launch `crw setup` after install + conversion-tuned copy ([5cc35f9](https://github.com/us/crw/commit/5cc35f915c54827481fbfc136d712928a84e66b0))
* **mcp:** add `install` command that wires skill + MCP for all agents ([4b41621](https://github.com/us/crw/commit/4b416214fb09930a22338521b089fa82683836df))


### Bug Fixes

* **ci:** rebase-retry docs-sync push + sync 0.17.0 changelog ([597aa90](https://github.com/us/crw/commit/597aa90b56ad5188691e844e29f5a849a54c092e))
* **install:** default to the crw CLI + drop the musl/glibc gate ([9958273](https://github.com/us/crw/commit/9958273bae8a5d954991b6a863dfee6ed7ef669a))
* **install:** drop the GB figure from local, surface 500 credits everywhere ([f997aa0](https://github.com/us/crw/commit/f997aa0e75007db2a4ee9fe4a4cc191bf00da0d0))
* **mcp:** clarify crawl/parse jsonSchema arg for MCP clients ([91be697](https://github.com/us/crw/commit/91be6977f88c259bdc2c846be92a183308d62ebe))
* **mcp:** static musl linux binaries + correct `claude mcp add` docs ([75333d6](https://github.com/us/crw/commit/75333d678e4112a1d03ca8d1c416fd989345cce0))
* **server:** make crw- model-prefix boot guard opt-in ([5cb867c](https://github.com/us/crw/commit/5cb867c17aeac45e03d30cd11897a7fc9a5b9e5f))
* **stealth:** bump UA + Sec-Ch-Ua from Chrome 131 to 150 ([695863c](https://github.com/us/crw/commit/695863cb09279e3bdee06928cbfb8b120a74e61b))

## [0.17.0](https://github.com/us/crw/compare/v0.16.0...v0.17.0) (2026-06-21)


### Features

* **renderer:** add opt-in Camoufox stealth renderer tier ([744beda](https://github.com/us/crw/commit/744beda540fad3e17b6d9c1d2127cad30ab83767))
* **renderer:** conditional hedge + event-driven readiness for p90 ([9065007](https://github.com/us/crw/commit/90650079d8f34e65cf67f22aad5bf96f62c580ef))
* **scrape:** add screenshot output format via CDP capture ([61e03e7](https://github.com/us/crw/commit/61e03e71b8dda137dffa730bcb864660009b270d)), closes [#161](https://github.com/us/crw/issues/161)
* **sdk:** add Research API methods to TS + Python SDKs ([3a2f710](https://github.com/us/crw/commit/3a2f71062b00acd0c67092fc5da7bb4caa1cf527))
* **search:** add Firecrawl-compatible research API engine layer ([ba1a87c](https://github.com/us/crw/commit/ba1a87c568da7f152f33cef63b188180ec210a29))
* **search:** overlap query-expansion scrape with original (C1) ([4f6147e](https://github.com/us/crw/commit/4f6147e8ef7ab3146387bad4cf0902e587d1660c))
* **skills:** add crw agent skill set ([0ddbe01](https://github.com/us/crw/commit/0ddbe010fdfe484f2c58db773569d85989441372))
* **skills:** publish crw-research agent skill + docs install command ([4638715](https://github.com/us/crw/commit/4638715bed7d34db0c3a08ec49be6b4a80f39791))


### Bug Fixes

* **docs:** render :::tabs and :::callouts in prerendered pages ([f3a495a](https://github.com/us/crw/commit/f3a495ac745598eddc83926466dede60fd7d4fa0))
* **map:** render SPA shells during URL discovery ([0ec4bf9](https://github.com/us/crw/commit/0ec4bf9b7c405e98def0b6f7ab3f73bf6320c275)), closes [#166](https://github.com/us/crw/issues/166)
* **mcp,sdk:** drop phantom search country param, export CrwApiError ([58b8e5c](https://github.com/us/crw/commit/58b8e5cd67347c3f4f7c8ebd341236873ba201b9))
* **pdf:** bound sandbox child address space to prevent false pdf_too_large ([06acb83](https://github.com/us/crw/commit/06acb8331490713967cf95bd40005c5af839373b))
* **proxy:** normalize empty CRW_CRAWLER__PROXY to None ([#154](https://github.com/us/crw/issues/154)) ([b3d0fe9](https://github.com/us/crw/commit/b3d0fe996a2d0e2b8f3231c55f6ed86f0b14552b))
* **scrape:** capture screenshot outside the nav-budget race ([4021b50](https://github.com/us/crw/commit/4021b5084db3b24b1f4c143f7729aa268006a039))
* **search:** resolve arXiv inspect via Semantic Scholar ([70126c6](https://github.com/us/crw/commit/70126c6e5e3c07fb7ad05400424adbdfdf129563))


### Performance

* **search:** research concurrency 4-&gt;8, cache cap 20k-&gt;3k ([e3a6ac3](https://github.com/us/crw/commit/e3a6ac3bef2a901a9f87b1cb710d594154ebe00f))

## [0.16.0](https://github.com/us/crw/compare/v0.15.2...v0.16.0) (2026-06-14)


### Features

* **mcp:** optimize MCP server for context, weight, and conformance ([aac7999](https://github.com/us/crw/commit/aac7999b9379fd8b6ef818ce37f78634416f79c1))
* **proxy:** add proxy list + rotation primitives and HTTP-path rotation ([776e9fb](https://github.com/us/crw/commit/776e9fbad4ee509ec5201020772169d151e0587f))
* **proxy:** rotate the JS/Chrome (CDP) path per request ([422ac09](https://github.com/us/crw/commit/422ac09c1f4dc2549a54e1079cec18330ccf0ea0))
* **proxy:** v2 BYOP plumbing + honest docs + verification harness ([0983ba3](https://github.com/us/crw/commit/0983ba38ad1537b21ecfdc658bdda7a79c0b5437))


### Bug Fixes

* **extract:** stop doubling /v1 in structured-extraction chat URL ([d8b8ebc](https://github.com/us/crw/commit/d8b8ebc887eca97ca75ae1eb1893429bc547e80e))
* **extract:** unify Anthropic structured URL, stop /v1/messages doubling ([90ce3dd](https://github.com/us/crw/commit/90ce3dd5479060eee0a3319e78d3d7653984e53c))
* **proxy:** accept snake_case proxy_list alias on v1 ScrapeRequest/CrawlRequest ([a8e7b71](https://github.com/us/crw/commit/a8e7b71d891355912dbfc1ad23a1d0e6f6bfd865))
* **proxy:** CLI crawl/map --proxy reaches the JS/CDP tier (round-4 review) ([6ee7175](https://github.com/us/crw/commit/6ee71755ce656b6ff32e4c1f4847e35655854035))
* **proxy:** resolve review findings (IP-leak/correctness hardening) ([72e0486](https://github.com/us/crw/commit/72e048683eceba19b676a4bca2c8186ad762d3e1))
* **proxy:** route /map discovery through the rotator (round-2 review) ([5ee06cf](https://github.com/us/crw/commit/5ee06cfb7e0c7e910a0ce0590597a277c6f49332))
* **proxy:** route crawl robots/sitemap egress through the rotator ([4835e7d](https://github.com/us/crw/commit/4835e7dc02263350ae7bbcf591a353258e308032))

## [0.15.2](https://github.com/us/crw/compare/v0.15.1...v0.15.2) (2026-06-12)


### Bug Fixes

* **mcp:** npm launcher downloads binary when platform pkg missing ([4ae1aa6](https://github.com/us/crw/commit/4ae1aa684f93c4c6a36fe157ce013700c5a9e6ea))

## [0.15.1](https://github.com/us/crw/compare/v0.15.0...v0.15.1) (2026-06-11)


### Bug Fixes

* **ci:** poll npm in verify_npm_sdk + expose ./package.json export ([ed185dd](https://github.com/us/crw/commit/ed185dd3fd34313e5babf1c9b71e08faab3918c5))

## [0.15.0](https://github.com/us/crw/compare/v0.14.0...v0.15.0) (2026-06-10)


### ⚠ BREAKING CHANGES

* CrwClient() with no API key now targets the cloud and raises if unauthenticated, instead of running locally. Set CRW_LOCAL=1 for the previous zero-config local behavior.

### Features

* add TypeScript SDK (crw-sdk) ([1dd96f7](https://github.com/us/crw/commit/1dd96f7037933f933773ef92d97b68af9134906a))
* cloud-first default + full client SDK parity ([21e819e](https://github.com/us/crw/commit/21e819ed8a863929bb67323c8b8c4b37ac5f712f))
* fold langchain/crewai adapters into crw.integrations extras ([cd49120](https://github.com/us/crw/commit/cd49120f957d23e06899d4628388e1aa8cd11842))
* **release:** publish crw-cli (and crw-browse) to crates.io ([b3d8004](https://github.com/us/crw/commit/b3d80046be72f1495985f6f21485c6fbdae4da7c))
* **search:** accept native SearXNG categories as passthrough ([b3bcc5d](https://github.com/us/crw/commit/b3bcc5d439fb37f214b76b96e053053765714e39))


### Bug Fixes

* **antibot:** detect Google rate-limit/bot-wall pages served with HTTP 200 ([08bb46e](https://github.com/us/crw/commit/08bb46e5e41d9933e1c9603239938dbd500323fe))
* **ci:** compile TS tests to JS so the SDK suite runs on Node 18/20 ([e1b7546](https://github.com/us/crw/commit/e1b75469af88cad612f93c64333508850864a981))
* **extract:** avoid doubling chat/completions in structured base_url ([3816bb6](https://github.com/us/crw/commit/3816bb6713dc0932d0b06179f2e37927dba95dfb))
* **map:** fold hyphen/underscore param spellings, add compare actions ([f34ae12](https://github.com/us/crw/commit/f34ae12844082db68d052b132c524eb5a4a4c9c1)), closes [#128](https://github.com/us/crw/issues/128)
* **npm:** declare node&gt;=18 engines on crw-mcp packages ([3c953fe](https://github.com/us/crw/commit/3c953feaa3c01d18f0dc05f36be339cb68d31470))
* **renderer:** reap CDP target + context on PoolGuard cancellation drop ([0eebadc](https://github.com/us/crw/commit/0eebadcb1a4360a0dad5b682301c181a0c328259))

## [0.14.0](https://github.com/us/crw/compare/v0.13.4...v0.14.0) (2026-06-08)


### Features

* **onboarding:** cloud-default messaging + api.fastcrw.com base URL ([4ccc92c](https://github.com/us/crw/commit/4ccc92c065027d900f0bd67e0273216bd76aa065))
* **pdf:** PDF→markdown via pdf-inspector with Firecrawl-compatible parsers + /v2/parse ([196b153](https://github.com/us/crw/commit/196b153db011807a80a2b19155ef0605a3ca692b))
* **search:** query-relevance rerank, list answers, multi-round latency guard ([ebafe83](https://github.com/us/crw/commit/ebafe8358283260df75a0d9629c59c8a1fcdad4a))


### Bug Fixes

* accept string env vars for auth.api_keys ([534f932](https://github.com/us/crw/commit/534f9320653c37aaba79e0db43ef9400b74b749b))
* **pdf:** vendor test fixture into each crate (preflight: no cross-crate include) ([2c7bfe9](https://github.com/us/crw/commit/2c7bfe93bbd38a384e84dab6daa5ae0fd2e55c65))
* **release:** scope out-of-crate include check to src/ only ([cd61fbf](https://github.com/us/crw/commit/cd61fbfd24046f6ef0a72e70e764189c39463fb6))
* **release:** verify apt/homebrew by artifact, not flaky status-ack ([7a572fd](https://github.com/us/crw/commit/7a572fd6e5b39117d316377919387e9e7ec8ad3b))

## [0.13.4](https://github.com/us/crw/compare/v0.13.3...v0.13.4) (2026-06-07)


### Bug Fixes

* **docker:** install aarch64 libc headers for cross-compile + CI guard ([8cd31ac](https://github.com/us/crw/commit/8cd31ac016177d3d2621e8ecfa03a027b1621a3a))

## [0.13.3](https://github.com/us/crw/compare/v0.13.2...v0.13.3) (2026-06-06)


### Bug Fixes

* **release:** stop verify-publish reporting false failures ([d57c8c6](https://github.com/us/crw/commit/d57c8c6285a014031bcf310386e83493c7dcd2f2))


### Performance

* **docker:** cross-compile arm64 instead of QEMU (2h -&gt; ~3min) ([a7cab42](https://github.com/us/crw/commit/a7cab423d11fd8a7474c587796f86eacf2c32df9))

## [0.13.2](https://github.com/us/crw/compare/v0.13.1...v0.13.2) (2026-06-06)


### Bug Fixes

* **server:** embed openapi spec inside the crate so it can publish ([e606cb9](https://github.com/us/crw/commit/e606cb9303cec3716b7dbffd4d848164486368ea))

## [0.13.1](https://github.com/us/crw/compare/v0.13.0...v0.13.1) (2026-06-06)


### Bug Fixes

* **release:** correct publish tier ordering + guard topology ([c84deed](https://github.com/us/crw/commit/c84deedeeeb2698f50d94247b770560cd6f3d0df))

## [0.13.0](https://github.com/us/crw/compare/v0.12.1...v0.13.0) (2026-06-06)


### Features

* **search:** deterministic Wikidata entity-relation lookup (W3) ([aa96e3e](https://github.com/us/crw/commit/aa96e3e182fad6d8e2caf03628461515ca5aab7f))


### Bug Fixes

* **release:** sync Cargo.lock internal crate versions to 0.12.1 ([b5fc8a5](https://github.com/us/crw/commit/b5fc8a5b988401bb2ecca6fdd3be328d1ccd683a))

## [0.12.1](https://github.com/us/crw/compare/v0.12.0...v0.12.1) (2026-06-05)


### Bug Fixes

* **release:** bump internal dep pins and track them in release config ([0139ec2](https://github.com/us/crw/commit/0139ec247c7f328ef172646eac3de5e0f287c42a))
* **release:** sync Cargo.lock with bumped internal dep versions ([5c89d60](https://github.com/us/crw/commit/5c89d6037e32435a49cfd4fbdf1c2ba05ad83ae8))

## [0.12.0](https://github.com/us/crw/compare/v0.11.0...v0.12.0) (2026-06-05)


### Features

* **answer:** gated moat-hardening abstention (answer_guarded) ([7ef7f32](https://github.com/us/crw/commit/7ef7f32c085f4e01429b4afa6794c52836cdd4e6))
* **mcp:** emit structuredContent for crw_search; bump protocol to 2025-06-18 ([0cd9a4f](https://github.com/us/crw/commit/0cd9a4fa0e338683b75c5719bc3c54cca3b2dba6)), closes [#89](https://github.com/us/crw/issues/89)
* **search:** diagnose search config and name unreachable host ([#90](https://github.com/us/crw/issues/90)) ([25f9441](https://github.com/us/crw/commit/25f94410869e24cb79a7835e2f05627d0eb07351))
* **search:** pin SearXNG infoboxes/answers as structured sources (W0) ([554f18c](https://github.com/us/crw/commit/554f18ce4747057a04ec64f9f983faf46a48dee2))


### Bug Fixes

* **search:** use resolvable searxng host in docker config ([#90](https://github.com/us/crw/issues/90)) ([d966021](https://github.com/us/crw/commit/d9660219d23bdd4364940c97c9911dd31e73567b))

## [0.11.0](https://github.com/us/crw/compare/v0.10.0...v0.11.0) (2026-06-03)


### Features

* **api:** serve /openapi.json and /openapi-3.0.json from crw-server ([3dc79b4](https://github.com/us/crw/commit/3dc79b443b6a41c3f325865aaa6bccdf562fda49))
* **docs:** ship OpenAPI spec, SKILL.md, and agent-shell ([2145c97](https://github.com/us/crw/commit/2145c977463b8d5577e5cf81d3c4106ea20eddb4))
* **docs:** wave 1 — API surface unblock for AI agent citations ([9b28090](https://github.com/us/crw/commit/9b2809028cf8c1c666fd94cd9b11dcab50daa633))
* **docs:** wave 2 — 15-page glossary cluster for AIO citations ([14cd7e1](https://github.com/us/crw/commit/14cd7e1db3f70cbd9dd7da08818b25d1e8688c71))
* **extract:** wave 2 cache token telemetry + DeepSeek provider tag fix ([1b4f1aa](https://github.com/us/crw/commit/1b4f1aa4eec70657964864f8c7d134150b6e2354))
* **mcp,cli,docs:** snippet alias + agent-shell polish for benchmark wins ([3472d13](https://github.com/us/crw/commit/3472d1333dd5520a481e226543e6271d940c145f))
* **monitor:** add feature-gated self-host crw-monitor mode (M6) ([ff732f3](https://github.com/us/crw/commit/ff732f3a31a66cf97746e79f7e4017788841f94a))
* **monitor:** add stateless change-tracking diff engine + LLM judge ([a078081](https://github.com/us/crw/commit/a07808127762b6d2acf248de65f0cb9d17aad2d6))
* **monitor:** stateless change-tracking diff engine + LLM judge + self-host monitor ([dc432ce](https://github.com/us/crw/commit/dc432cef74e720afb4e8224bef7647212f220ed7))
* **renderer:** add chrome_proxy tier + antibot-driven failover ([0e37e30](https://github.com/us/crw/commit/0e37e307b7dbe2bb52b0e4ef5b190fd2c1ec0217))
* **search:** adaptive multi-round evidence-scout retrieval (gated) ([9dd3224](https://github.com/us/crw/commit/9dd32244eb3d072852f601ddfd538c019074c4b5))
* **search:** commit-policy answer prompt to cut over-abstention ([#71](https://github.com/us/crw/issues/71)) ([cac7b29](https://github.com/us/crw/commit/cac7b29cbc78f358b3d4cc508ad9d97e250f8483))
* **search:** gated calibrated-answer path to cut over-abstention ([#78](https://github.com/us/crw/issues/78)) ([a53225d](https://github.com/us/crw/commit/a53225d799082751dfe933b8fb3bae4c16d07a90))
* **search:** gated page-2 fallback for thin reranked answer pools ([#77](https://github.com/us/crw/issues/77)) ([2a7f0ac](https://github.com/us/crw/commit/2a7f0ac247666e9e360dad4bc0f26da247d4355e))
* **search:** gated synthesis temperature/seed for deterministic eval ([#81](https://github.com/us/crw/issues/81)) ([52a8c58](https://github.com/us/crw/commit/52a8c58edf3e81c84e8ca8317e46d8ac95a8bf94))
* **search:** multi-query expansion for the answer path (gated, default off) ([#73](https://github.com/us/crw/issues/73)) ([4e387af](https://github.com/us/crw/commit/4e387afc0f3e979ce21b30842539e7e1b9b774df))
* **search:** multi-variant query expansion for recall (gated) ([7bd093f](https://github.com/us/crw/commit/7bd093fa674c933b7c4206bbefb32cc2414bb471))
* **search:** passage-level relevance gate for the answer path (gated, off) ([#75](https://github.com/us/crw/issues/75)) ([43ac625](https://github.com/us/crw/commit/43ac625caf60d49fa6ca6bfa7fdd54ad8a12418e))
* **search:** RRF re-rank + junk/coverage/geo filter + query cleaning for answer path ([32efee6](https://github.com/us/crw/commit/32efee67ed4e180505de9e2aec05d489adc58a51))
* **search:** RRF re-rank + junk/coverage/geo filter + query cleaning for answer path ([682fa1c](https://github.com/us/crw/commit/682fa1ccbe26a0cd919a2181831ad1c73b530642))
* **search:** snippet-fallback (Pattern A) + bake calibrated-answer durable ([#79](https://github.com/us/crw/issues/79)) ([de1b1f1](https://github.com/us/crw/commit/de1b1f1eac4108095b3d6d7bdda97c035666102a))
* **search:** wave 4 R1+R2 — aggregated llmUsage + per-leg max_tokens (v0.11.0) ([3570f82](https://github.com/us/crw/commit/3570f82e0e1fe00e9851dc405506bdd2f0e04c01))
* **search:** wave 4 R1+R2 + bump 0.11.0 ([9ebd23d](https://github.com/us/crw/commit/9ebd23d9d658fb07700ffc4a3259fbe079763627))
* **v2:** add Firecrawl /v2 API surface ([f793ec0](https://github.com/us/crw/commit/f793ec09c8c95dad142022f9579adb4c4e1cceb6))


### Bug Fixes

* **antibot:** strip inline data-URIs before classifier deep scan ([6f1cbd2](https://github.com/us/crw/commit/6f1cbd20b98f73df099f72b7ad08e41acada2a7e))
* **browse:** address outbound hardening review feedback ([a3c2076](https://github.com/us/crw/commit/a3c2076ffdd8024579bf7ab97653816cb5bd3881))
* **browse:** harden outbound URL handling ([c370972](https://github.com/us/crw/commit/c37097253c7df89a5cedb3ddb79e5c9386f75b1b))
* **cli:** swap SearXNG default to 127.0.0.1 + actionable error hint ([618d41b](https://github.com/us/crw/commit/618d41b52c3fa0c971eb6c34ea98922f06ea094f))
* **docker:** include OpenAPI specs in crw-api build context ([f70c7af](https://github.com/us/crw/commit/f70c7af6486a82d6bd86c9a7c1054c410f045877))
* **docker:** include OpenAPI specs in crw-api build context ([0238138](https://github.com/us/crw/commit/0238138f832a507a70ed411d96ec0143cbb7333e))
* **docs:** add 2 missing Firecrawl-shape shims caught by sapient ([22b3d54](https://github.com/us/crw/commit/22b3d5482ca224d0eebd76a795ed8428f108d080))
* **extract:** accept deepseek/openai-compatible providers in structured extract ([2f0d86d](https://github.com/us/crw/commit/2f0d86d5245c24d388671299beb5acd76458c4ca))
* **search:** default reranker to the proven lexical core ([#69](https://github.com/us/crw/issues/69)) ([06585b6](https://github.com/us/crw/commit/06585b60abd2093288e520157369f16286fd2737))
* **search:** point engine at searxng-internal alias (search_rpc-only) ([#70](https://github.com/us/crw/issues/70)) ([110aee7](https://github.com/us/crw/commit/110aee729624b42d71b6f219f52dadd6d6bc6ddb))
* **v2:** batch status mutates state in place (no O(n^2) snapshot copy) ([c03d359](https://github.com/us/crw/commit/c03d359cfdfc2b2cf4d9603cccff7a08e6c54c69))
* **v2:** emit invalidURLs (not invalidUrls) on batch start ([611c54d](https://github.com/us/crw/commit/611c54ded77aaa962cb76ff504c11310c7005825))


### Performance

* **search:** calibrated answer top_n 8-&gt;5 (4x faster, accuracy holds) ([#80](https://github.com/us/crw/issues/80)) ([751f8ee](https://github.com/us/crw/commit/751f8ee160d28b84d352e533864ca4043793d294))

## [0.10.0](https://github.com/us/crw/compare/v0.9.1...v0.10.0) (2026-05-20)


### Features

* **detector:** add vendor-specific anti-bot block markers ([c88c508](https://github.com/us/crw/commit/c88c508fb90b166dfe3727fd5dfb4f1597e43667))
* **renderer:** add chrome_proxy as 4th fallback tier ([b4da4f7](https://github.com/us/crw/commit/b4da4f79bb4d0ed71c25f14aaae5137d00f8b26b))
* **renderer:** per-request country via CDP proxy auth ([11b4d32](https://github.com/us/crw/commit/11b4d32285ed8a4e6bee8f390c264f9fc0be1b1a))


### Bug Fixes

* **release:** harden npm publish + fix mcp-registry verifier ([9d4076f](https://github.com/us/crw/commit/9d4076fadd252a33e7887ee6e4925be8e6aa7d8e))
* **renderer:** detect CloudFront/WAF 403 as bot-wall ([7e058b2](https://github.com/us/crw/commit/7e058b2915eff8b36d1186013e0810b2290492f4))
* **renderer:** escalate JS tier on 4xx/5xx and vendor-detected blocks ([648c372](https://github.com/us/crw/commit/648c372ee5d52aed1459c22725be2e6d34d95afb))

## [0.9.1](https://github.com/us/crw/compare/v0.9.0...v0.9.1) (2026-05-16)


### Bug Fixes

* **release:** sync crw-cli internal dep versions with workspace ([26c528e](https://github.com/us/crw/commit/26c528e737c5ed136d3c6e72da36b9324363805f))

## [0.9.0](https://github.com/us/crw/compare/v0.8.3...v0.9.0) (2026-05-16)


### Features

* **cli:** add AI extraction flags and `crw setup --reset` ([912eea0](https://github.com/us/crw/commit/912eea0a1fa0db5a838560f2c25da44b3ee33d44))

## [0.8.3](https://github.com/us/crw/compare/v0.8.2...v0.8.3) (2026-05-15)


### Features

* **cli:** two-phase auto-fallback for `crw <url>` scrape ([a871e54](https://github.com/us/crw/commit/a871e544443673751974a8dc5ebb4f2b0eafd59f))
* **setup:** make config.toml the canonical source for `crw setup` ([b07c154](https://github.com/us/crw/commit/b07c1549932399a355d30e15632d424f0ad28b85))


### Miscellaneous

* release 0.8.2 ([38ae764](https://github.com/us/crw/commit/38ae7641b44d321150febc38eda530db3d572046))
* release 0.8.3 ([efba1b3](https://github.com/us/crw/commit/efba1b308d6ac3a5e7a5bbd3f132514bf631e86b))

## [0.8.2](https://github.com/us/crw/compare/v0.8.2...v0.8.2) (2026-05-15)


### Features

* **cli:** two-phase auto-fallback for `crw <url>` scrape ([a871e54](https://github.com/us/crw/commit/a871e544443673751974a8dc5ebb4f2b0eafd59f))
* **setup:** make config.toml the canonical source for `crw setup` ([b07c154](https://github.com/us/crw/commit/b07c1549932399a355d30e15632d424f0ad28b85))


### Miscellaneous

* release 0.8.2 ([38ae764](https://github.com/us/crw/commit/38ae7641b44d321150febc38eda530db3d572046))

## [0.8.2](https://github.com/us/crw/compare/v0.8.1...v0.8.2) (2026-05-14)


### Bug Fixes

* **release:** move crw-cli to unpublished and update dep versions ([7f121f6](https://github.com/us/crw/commit/7f121f6f731e7bfa311be48fa442e0049bbda16d))

## [0.8.1](https://github.com/us/crw/compare/v0.8.0...v0.8.1) (2026-05-14)


### Bug Fixes

* **cli:** mark crw-cli as publish=false to fix release ([3104cc5](https://github.com/us/crw/commit/3104cc5acec6d6a0307adbb5cc9897ce04417a16))

## [0.8.0](https://github.com/us/crw/compare/v0.7.1...v0.8.0) (2026-05-14)


### Features

* **cli:** add interactive setup wizard ([a5613b9](https://github.com/us/crw/commit/a5613b965d3444ab0ad214b976cbf6e56747e523))

## [0.7.1](https://github.com/us/crw/compare/v0.7.0...v0.7.1) (2026-05-12)


### Bug Fixes

* bump stale internal version pins to 0.7.0 ([#48](https://github.com/us/crw/issues/48)) ([0bec22a](https://github.com/us/crw/commit/0bec22a76e03f428e28281b30bb34364bf5e5edd))

## [0.7.0](https://github.com/us/crw/compare/v0.6.4...v0.7.0) (2026-05-12)


### Features

* LLM summary and search answer (BYOK) ([#45](https://github.com/us/crw/issues/45)) ([ffcc2a5](https://github.com/us/crw/commit/ffcc2a52cd273dc334dd46b663be24fd45df4711))

## [0.6.4](https://github.com/us/crw/compare/v0.6.3...v0.6.4) (2026-05-12)


### Features

* **renderer:** add bounded browser-context pool for Chrome tier ([#43](https://github.com/us/crw/issues/43)) ([69b4861](https://github.com/us/crw/commit/69b48610d269c37fa043bcd58855b3970f554f94))

## [0.6.3](https://github.com/us/crw/compare/v0.6.2...v0.6.3) (2026-05-12)


### Features

* **map:** drop action URLs and strip tracking params (closes [#40](https://github.com/us/crw/issues/40)) ([#41](https://github.com/us/crw/issues/41)) ([6d9ed39](https://github.com/us/crw/commit/6d9ed39f67e4d92cc51ca16ccc21e451c4bb0373))

## [0.6.2](https://github.com/us/crw/compare/v0.6.1...v0.6.2) (2026-05-10)


### Features

* **search:** add /v1/search endpoint backed by bundled SearXNG sidecar ([f4bd7f4](https://github.com/us/crw/commit/f4bd7f46db9f286e3c49be95a968951802a90710))


### Bug Fixes

* **antibot:** drop bare 'captcha'/'access denied' markers — false positives ([fae6c09](https://github.com/us/crw/commit/fae6c09537cf286bfb08cb9ebefab0c723c4160f))
* **crawl:** drop redundant `.into_iter()` for clippy 1.95 ([#39](https://github.com/us/crw/issues/39)) ([fb4032b](https://github.com/us/crw/commit/fb4032b86a5c0095e7e69b198ac2017aa7003000))
* **map:** WordPress sitemap-index timeout (closes [#33](https://github.com/us/crw/issues/33)) ([c3dfd6c](https://github.com/us/crw/commit/c3dfd6c66ff6bdbbaf3d2ce1646dbb9d7ac6dd5a))
* **release:** register crw-search crate in release manifest ([9074761](https://github.com/us/crw/commit/907476163c69c5fd8e421e5063686a750c10ce24))
* **search:** codex iteration-1 hardening — error mapping, resource bounds, container ([5acba7b](https://github.com/us/crw/commit/5acba7bbf1be2cc147e5c25ad9fca80e9bce757d))
* **search:** codex iteration-2 — error-body cap, per-source row budget, doc ([a440d6e](https://github.com/us/crw/commit/a440d6e4ae3b6ac0dc8decdead3efe04ad33bc43))
* **search:** codex iteration-3 — predicate-based well-formed filter ([4b4df3a](https://github.com/us/crw/commit/4b4df3a521cba31cd2a82f2d3595d726cb4b2c16))
* **search:** use real SearXNG image tag and add fallback secret_key ([be1f403](https://github.com/us/crw/commit/be1f403648d352bc68037b0c5ba208729b96d1fa))

## [0.6.1](https://github.com/us/crw/compare/v0.6.0...v0.6.1) (2026-05-09)


### Features

* **metrics:** cdp_pending_requests, cdp_live_connections, ([b5f7bec](https://github.com/us/crw/commit/b5f7bec28308e4f0094b7bbcbbecc5d2f734e385))
* **renderer:** live-connection registry + 60s telemetry sampler ([b5f7bec](https://github.com/us/crw/commit/b5f7bec28308e4f0094b7bbcbbecc5d2f734e385))
* **renderer:** target lifecycle metric + leaked detection ([b5f7bec](https://github.com/us/crw/commit/b5f7bec28308e4f0094b7bbcbbecc5d2f734e385))
* **server:** /ready endpoint with deep status code ([b5f7bec](https://github.com/us/crw/commit/b5f7bec28308e4f0094b7bbcbbecc5d2f734e385))


### Bug Fixes

* **release:** bulletproof publish pipeline and drop pdf feature ([8fcf2f6](https://github.com/us/crw/commit/8fcf2f656aabe1a8a05db7d6c21011e06959e184))
* **renderer:** invalidate cached chrome WS URL on connect failure ([b5f7bec](https://github.com/us/crw/commit/b5f7bec28308e4f0094b7bbcbbecc5d2f734e385))

## [0.6.0](https://github.com/us/crw/compare/v0.5.0...v0.6.0) (2026-05-09)


### Features

* **extract:** scale recall to 63.74% on 1000-URL benchmark ([5b85555](https://github.com/us/crw/commit/5b855554c5f7ba16981fbe2060e25cca4ba81686))
* **renderer:** add browserless/chromium opt-in stealth profile (+2.5pt) ([d2414c9](https://github.com/us/crw/commit/d2414c9cd89dc01447b9e52501aa26180ce7d326))
* **renderer:** chrome-stealth wiring + CDP discovery improvements ([6b2e77c](https://github.com/us/crw/commit/6b2e77c2a356ef8fc453560870985819ce75483a))
* **server,core,crawl:** plumb tier timeouts and recall pipeline ([7cbee43](https://github.com/us/crw/commit/7cbee43e5db319f0af39dddea07faccbf0cd25ee))


### Miscellaneous

* release 0.6.0 ([bd03a35](https://github.com/us/crw/commit/bd03a352922b293431e49722c052f90c945f1c56))

## [0.5.0](https://github.com/us/crw/compare/v0.4.2...v0.5.0) (2026-05-04)


### Features

* **core:** add deadline module and request/renderer config scaffolding ([5a4e69a](https://github.com/us/crw/commit/5a4e69ae605d15c0090f3d866db0f8f4fa23a715))
* **core:** thread end-to-end Deadline through scrape pipeline ([5991986](https://github.com/us/crw/commit/5991986cdac9756500dab40b8bf05ad454dbd21c))
* **crawl:** key per-domain rate limiter by eTLD+1 ([39c7954](https://github.com/us/crw/commit/39c7954881cfde47e34dad2eaa4141f1f10b1156))
* **crawl:** per-host concurrency cap on the eTLD+1 limiter ([274f462](https://github.com/us/crw/commit/274f462b2755a02fc2485bddc7ac8ad3fd11c0e3))
* **renderer:** add browserless/chromium opt-in stealth profile ([236f626](https://github.com/us/crw/commit/236f62682f29011a959bdef5a9770475a809f0a9))
* **renderer:** chrome nav-budget cap + truncated/deadline_exceeded flags ([c57cef8](https://github.com/us/crw/commit/c57cef8c6ad2ba2fefce7f4110685bc779359378))
* **renderer:** chrome request-paused interception pump (T27) ([13fcaa4](https://github.com/us/crw/commit/13fcaa4c5560f254d67682bff96ba24e39cdf13e))
* **renderer:** leak-through fallback when global breaker open & host clean ([86a9e36](https://github.com/us/crw/commit/86a9e36880f2dddcb3d7b7bd6c993825559cf487))
* **renderer:** outcome-aware breaker + extraction and stealth fixes ([86dd10f](https://github.com/us/crw/commit/86dd10fd014235cb9bd107e32c7cf6e04cb03367))
* **renderer:** own per-eTLD+1 host limiter in FallbackRenderer ([0577516](https://github.com/us/crw/commit/0577516bd41dc284f24cbaf3ed95544504ba50be))
* **renderer:** recover FC-wins URLs to reach 92% bench coverage ([ba12424](https://github.com/us/crw/commit/ba12424e44c34aa44bb8a41bc1f16d1dd87f498a))


### Bug Fixes

* **compose:** auto-restart and bound memory for renderer containers ([dd610cc](https://github.com/us/crw/commit/dd610ccae2579138d5438795e1d5ac441a0fafc3))
* **core:** emit meaningful Timeout value when deadline already expired ([607bb27](https://github.com/us/crw/commit/607bb27692686f3563af52ea721d7dfb800d0405))
* **crawl:** prioritize anti-bot detection over placeholder warning ([05aa933](https://github.com/us/crw/commit/05aa93358f3fa9826eb97db114bef06d1754dae3))
* escalate to JS renderer on HTTP failure and empty markdown ([9fc7934](https://github.com/us/crw/commit/9fc79344702e30be0555e63a02aa5377f15cca93))
* **mcp:** apply per-endpoint timeouts to proxy client ([741f1b2](https://github.com/us/crw/commit/741f1b245e064b267b4fb0dfb5487099bc86e2e4))
* **renderer:** enforce Deadline in HttpFetcher via tokio::time::timeout ([b1c4058](https://github.com/us/crw/commit/b1c4058f47eed204d413a51c56d8ae43f547ff63))
* **renderer:** keep larger thin-result HTML when stitching attempts ([8147236](https://github.com/us/crw/commit/8147236cc6b94d3c2db34f1128a687d9e110dc35))
* **renderer:** rescue 39 bench failures via UA, retry, and thin-content escalation ([ddacb49](https://github.com/us/crw/commit/ddacb49e92688c3a20c7f7fe32da58d83c620f31))
* **server:** classify anti-bot challenges as anti_bot, not no-markdown ([3ece4dd](https://github.com/us/crw/commit/3ece4dd5b5318f71fe3744fa9d09948afaa738de))


### Performance

* **renderer:** drop fixed 2s JS wait, rely on SPA selector poll ([cb043f7](https://github.com/us/crw/commit/cb043f7754f870df67a3a56e41a552ba7f7867f4))
* **renderer:** tighten tier timeouts and bump LP retry threshold ([3f93d60](https://github.com/us/crw/commit/3f93d6052251eb72abee20ea4992ca3cdfc7ddb4))
* **renderer:** widen breaker tolerance to 20 failures / 10s cooldown ([6525a84](https://github.com/us/crw/commit/6525a84c18e2c4fafa92cc0d29203310755d3ef1))


### Miscellaneous

* release 0.5.0 ([3987de1](https://github.com/us/crw/commit/3987de1b15b5d7605cc26645d14b74020c8eb7a9))

## [0.4.2](https://github.com/us/crw/compare/v0.4.1...v0.4.2) (2026-04-29)


### Features

* **core:** add render decision types and prometheus metrics scaffold ([e08682b](https://github.com/us/crw/commit/e08682b761822a7100e0f40cffe4cd4f3dcf2a5c))
* **renderer:** add per-host renderer preference cache ([21e41d1](https://github.com/us/crw/commit/21e41d1330bed255854824f55c3419a590a86411))
* **renderer:** track HTTP routing and warn on pinned-renderer failure ([3208d27](https://github.com/us/crw/commit/3208d277aa4c480c1257648dc86a7151dcfb8976))
* **renderer:** wire host preferences, circuit breakers, and CF detection ([0c53c64](https://github.com/us/crw/commit/0c53c645562c43a4f62aa22f1e7f603c42b3b3f3))


### Bug Fixes

* **core,renderer:** surface render metadata and harden host normalization ([ee4130b](https://github.com/us/crw/commit/ee4130b62467defb61c5b85bf267c767b3bd909a))
* **renderer:** correct failure classification and routing decisions ([4d684bd](https://github.com/us/crw/commit/4d684bdaa0cd1a27f011d73322099258a0f713be))
* **renderer:** probe lifecycle, RAII guard, breaker counter ([02044f5](https://github.com/us/crw/commit/02044f573cd6274231b3856cd799d7e74d61f9ba))

## [0.4.1](https://github.com/us/crw/compare/v0.4.0...v0.4.1) (2026-04-28)


### Features

* add per-request renderer field for scrape and crawl APIs ([#29](https://github.com/us/crw/issues/29)) ([f1e0b63](https://github.com/us/crw/commit/f1e0b63fd28be0ceb38342086a309f92bbbc1e53))
* **crw-browse:** add interactive browser MCP server with phase-2 tools ([e78879d](https://github.com/us/crw/commit/e78879db18c7c4b3df2a4984349a65b4493b1cda))
* honor renderer mode and force_js in config (fixes [#28](https://github.com/us/crw/issues/28)) ([b76e473](https://github.com/us/crw/commit/b76e473facbce08a841ef8bd9fdfac97a552a8fd))


### Bug Fixes

* detect failed JS renders and fail over to next renderer ([fca8fd5](https://github.com/us/crw/commit/fca8fd5cadb4fa3c96bf5a315f96ab6d1e63989c))
* **docs:** use absolute logo paths in site.config.js ([c5c9321](https://github.com/us/crw/commit/c5c93215561094a35038ab6af2b21e91c16199f4))
* **docs:** use absolute paths for logo and favicon assets ([cdb1451](https://github.com/us/crw/commit/cdb14517da6425c105b95749dec35bbc9e977f5e))

## [0.4.0](https://github.com/us/crw/compare/v0.3.6...v0.4.0) (2026-04-22)


### Features

* add crw-browse MCP server, SOCKS5 proxy, extract mcp-proto ([9a53753](https://github.com/us/crw/commit/9a53753baf6d87272bd2417fc87102a8ed34d41b))


### Miscellaneous

* release 0.4.0 ([e15fc74](https://github.com/us/crw/commit/e15fc74cf0dfc7c02ca7e6b82258aeff57f74f17))

## [0.3.6](https://github.com/us/crw/compare/v0.3.5...v0.3.6) (2026-04-21)


### Features

* **ci:** add Google Indexing API notification for docs changes ([3b5a340](https://github.com/us/crw/commit/3b5a3404e91a1d776275ac312ad08cad86339a98))
* **docs:** generate static HTML pages for SEO indexability ([7b321c0](https://github.com/us/crw/commit/7b321c0a26cea0da32d42e952f6327b468bdb099))


### Bug Fixes

* **ci:** trigger release workflow after release-please creates tag ([27f2b67](https://github.com/us/crw/commit/27f2b67d0b9db4f7b1bacc6c901e9c92131a3a95))
* **mcp:** bump npm optionalDependencies from 0.3.0 to 0.3.5 ([0e363e0](https://github.com/us/crw/commit/0e363e0fc512eb18bcb8284a9723b00f50e2dfd0))
* **renderer:** detect loading placeholders and poll for content stability ([d3b642b](https://github.com/us/crw/commit/d3b642b2736b4568fa8a3502e521b8bede60317f))

## [0.3.5](https://github.com/us/crw/compare/v0.3.4...v0.3.5) (2026-04-09)


### Features

* **mcp:** add crw_search tool for cloud/proxy mode ([7fe4a8e](https://github.com/us/crw/commit/7fe4a8e79aae9dfcd0b400c17f175af583a33eef))

## [0.3.4](https://github.com/us/crw/compare/v0.3.3...v0.3.4) (2026-04-09)


### Bug Fixes

* **config:** env var overrides not applied due to missing prefix_separator ([71c7ae5](https://github.com/us/crw/commit/71c7ae544bd74575cf30101190d65708934bcb1f)), closes [#18](https://github.com/us/crw/issues/18)

## [0.3.3](https://github.com/us/crw/compare/v0.3.2...v0.3.3) (2026-04-09)


### Features

* add APT/Debian package distribution ([c34b8e9](https://github.com/us/crw/commit/c34b8e93563c0f3f4b3fa1a1e395e6041e97766c))
* **renderer:** spawn all available browsers for multi-renderer fallback ([f546437](https://github.com/us/crw/commit/f546437568eb833684978793e5b79880f2710016))

## [0.3.2](https://github.com/us/crw/compare/v0.3.1...v0.3.2) (2026-04-08)


### Bug Fixes

* **cli:** auto-prepend https:// when no scheme provided ([1050606](https://github.com/us/crw/commit/105060644acb1ef930555869372f5413947ac194))

## [0.3.1](https://github.com/us/crw/compare/v0.3.0...v0.3.1) (2026-04-08)


### Features

* add llms.txt, SKILL.md, MCP init command, and docs UI improvements ([1b22d19](https://github.com/us/crw/commit/1b22d194ec9a8b8db8db39a1d44ce38a56716f5d))
* add one-line install script with auto platform detection ([6354f79](https://github.com/us/crw/commit/6354f7929896bb9a12eeebb9d5bf439731fac874))
* **docs:** add dark mode logo support and improve docs UI ([047df7b](https://github.com/us/crw/commit/047df7b46485d9cdec76b213db181dc0aa592511))
* **docs:** align design with SaaS site and update branding ([631d07c](https://github.com/us/crw/commit/631d07c66a34bcc71e8d2494a177182977886a8c))
* **docs:** unify docs into docs.fastcrw.com with Mintlify-style design ([4994998](https://github.com/us/crw/commit/4994998ae97924100e4415b90a5b78db3b8cf09e))
* **docs:** update URLs, dark mode, syntax highlighting, and benchmarks ([0678cdf](https://github.com/us/crw/commit/0678cdfde6a9de9b5fa56ec2c6256a63a0342317))
* release all 3 binaries, CLI auto-browser, README overhaul ([aa2950d](https://github.com/us/crw/commit/aa2950d564cb97f668c0c838713f612bffa33e32))
* update README banner with new logo ([bcba1ad](https://github.com/us/crw/commit/bcba1ad95e7ac1afd3066827f2327452b60c81f0))


### Bug Fixes

* crawl HTTP polling bug + SDK test suite + docs ([#16](https://github.com/us/crw/issues/16)) ([b6d8983](https://github.com/us/crw/commit/b6d898360cb2396683e9e82f778d2ee3b8455625))
* remove internal implementation detail from roadmap ([a5013f0](https://github.com/us/crw/commit/a5013f07f039615de5c5988e6bbf64629b56aa0e))

## [0.3.0](https://github.com/us/crw/compare/v0.2.2...v0.3.0) (2026-04-02)


### Features

* add search() method to Python SDK and docs ([591e3fe](https://github.com/us/crw/commit/591e3fed4bbd14b4470fd2f5cbc24c02f7543dba))

## [0.2.2](https://github.com/us/crw/compare/v0.2.1...v0.2.2) (2026-04-02)


### Bug Fixes

* **renderer:** escalate to JS renderer on HTTP 401/403 responses ([f515caa](https://github.com/us/crw/commit/f515caa2e315a06df9274967bdc3fb23dbafcbcf))
* use GitHub latest release instead of pinned version for binary download ([4afcb1a](https://github.com/us/crw/commit/4afcb1a4c67d4e9b36809ed32ce88b6a9fd4c342))

## [0.2.1](https://github.com/us/crw/compare/v0.2.0...v0.2.1) (2026-03-28)


### Bug Fixes

* make crw-mcp npm wrapper executable ([576a9eb](https://github.com/us/crw/commit/576a9eb19ae90bc677344045dd70fb96e8b938da))
* use latest tag in server.json OCI identifier ([7ec3b82](https://github.com/us/crw/commit/7ec3b82ab78aa2c7ff900d07400f8a2426cb955f))

## [0.2.0](https://github.com/us/crw/compare/v0.1.2...v0.2.0) (2026-03-28)


### Features

* add MCP Registry support for official server discovery ([154b9f5](https://github.com/us/crw/commit/154b9f520015260d012ebf899513c3af1f9dfe3d))

## [0.1.2](https://github.com/us/crw/compare/v0.1.1...v0.1.2) (2026-03-27)


### Bug Fixes

* vendor pdf-inspector as crw-pdf for crates.io publishability ([3f7681d](https://github.com/us/crw/commit/3f7681dde0b78ff2c0c11d232d21a714edb94d75))

## [0.1.1](https://github.com/us/crw/compare/v0.1.0...v0.1.1) (2026-03-26)


### Bug Fixes

* skip already-published crates without masking real errors ([010649c](https://github.com/us/crw/commit/010649c522e4e49edcf51873d4c1065e08d510b1))

## [0.1.0](https://github.com/us/crw/compare/v0.0.14...v0.1.0) (2026-03-26)


### Features

* add PDF extraction support via pdf-inspector ([06dd5bf](https://github.com/us/crw/commit/06dd5bf89caf004929f22d41d00f8a297d09b825))

## [0.0.14](https://github.com/us/crw/compare/v0.0.13...v0.0.14) (2026-03-25)


### Features

* **mcp:** auto-download LightPanda binary for zero-config JS rendering ([41f443b](https://github.com/us/crw/commit/41f443b885326401b653cfcba0054cb943672ca6))
* **mcp:** auto-spawn headless Chrome for JS rendering in embedded mode ([9a6b0ae](https://github.com/us/crw/commit/9a6b0ae3f16399f8f9f233109e431f74a882d973))


### Bug Fixes

* **ci:** move crw-mcp to Tier 4 in release workflow and add workflow_dispatch ([d7584a8](https://github.com/us/crw/commit/d7584a82c0dd4ac0c6cd8b169b19939d92eb4e95))

## [0.0.13](https://github.com/us/crw/compare/v0.0.12...v0.0.13) (2026-03-24)


### Features

* **mcp:** add embedded mode — self-contained MCP server, no crw-server needed ([75e5450](https://github.com/us/crw/commit/75e54504487f24ee30c0272bb83eb9aab807a284))


### Bug Fixes

* **ci:** switch release-please to simple type for Rust workspace support ([51cd420](https://github.com/us/crw/commit/51cd420ab77e4bd58bf1a6a7ab0c28287896a0b7))

## v0.0.12

- **Readability drill-down** — when `<main>` or `<article>` wraps >90% of body, the extractor now searches inside for narrower content elements (`.main-page-content`, `.article-content`, `.entry-content`, etc.) instead of discarding. Fixes MDN pages returning 35 chars and StackOverflow returning only the question
- **Base64 image stripping** — `data:` URI images are removed in both HTML cleaning (lol_html) and markdown post-processing (regex safety net). Eliminates massive base64 blobs from Reddit and similar sites
- **Select/dropdown removal** — `<select>` elements removed in `onlyMainContent` mode; dropdown/city-selector/location-selector noise patterns added. Fixes Hürriyet city dropdown leaking into content
- **Extended scored selectors** — added `.main-page-content`, `.js-post-body`, `.s-prose`, `#question`, `.page-content`, `#page-content`, `[role="article"]` for better MDN, StackOverflow, and generic site coverage
- **Smarter fallback chain** — when primary extraction produces too-short markdown, both fallbacks (cleaned HTML and basic clean) are tried and the longer result is picked, instead of short-circuiting on non-empty but insufficient content

## v0.0.11

- **Stealth anti-bot bypass** — automatic stealth JS injection via `Page.addScriptToEvaluateOnNewDocument` before every CDP navigation. Spoofs `navigator.webdriver`, Chrome runtime object, plugins array, languages, permissions API, iframe `contentWindow`, and `toString()` proxy to bypass Cloudflare, PerimeterX, and other bot detection systems
- **Cloudflare challenge auto-retry** — detects Cloudflare JS challenge pages ("Just a moment", `cf-browser-verification`, `challenge-platform`) after page load and polls up to 3 times at 3-second intervals for non-interactive challenges to auto-resolve
- **HTTP → CDP auto-escalation** — `FallbackRenderer::fetch()` in auto mode now checks HTTP responses for anti-bot challenge signatures and automatically escalates to JS rendering when detected, instead of returning the challenge HTML
- **Chrome failover in Docker** — full automatic failover chain: HTTP → LightPanda → Chrome. Added `chromedp/headless-shell` as a Docker Compose sidecar service with 2GB shared memory. If LightPanda crashes on complex SPAs (React, Angular), Chrome handles the render
- **Chrome WS URL auto-discovery** — CDP renderer resolves Chrome DevTools WebSocket URL via the `/json/version` HTTP endpoint with `Host: localhost` header (required for chromedp/headless-shell's socat proxy). Uses `OnceCell` for lazy one-time resolution
- **Proxy configuration docs** — expanded proxy config comments with examples for HTTP, SOCKS5, and residential proxy providers (IPRoyal, Oxylabs, Smartproxy)
- **Raw string delimiter fix** — fixed `markdown.rs` test that used `r#"..."#` with a string containing `"#`, changed to `r##"..."##`

## v0.0.10 / v0.0.9

- **Crawl cancel endpoint** — `DELETE /v1/crawl/{id}` cancels a running crawl job via `AbortHandle` and returns `{ success: true }`
- **API rate limiting** — token-bucket rate limiter (configurable `rate_limit_rps`, default 10). Returns 429 with `error_code: "rate_limited"` when exceeded
- **Machine-readable error codes** — all error responses now include an `error_code` field (e.g. `"invalid_url"`, `"http_error"`, `"rate_limited"`, `"not_found"`)
- **Map response envelope** — `/v1/map` now returns `{ success, data: { links } }` instead of `{ success, links }` for consistency with other endpoints
- **Fenced code blocks** — indented code blocks (4-space) are post-processed into fenced (```) blocks for better LLM/RAG compatibility
- **Sphinx footer cleanup** — `"footer"` added to exact-token noise patterns, catching `<div class="footer">` in Sphinx/documentation sites
- **`renderedWith: "http"`** — HTTP-only fetches now report `rendered_with: "http"` in metadata instead of `null`
- **405 JSON responses** — all routes now have `.fallback(method_not_allowed)` returning structured JSON with `error_code: "method_not_allowed"` instead of empty bodies
- **Anchor link cleanup** — empty anchor links (`[](#id)`, `[¶](#id)`) and pilcrow/section signs stripped from Markdown output
- **`role="contentinfo"` cleanup** — elements with ARIA roles `contentinfo`, `navigation`, `banner`, `complementary` removed during cleaning
- **Tiny chunk merging** — topic chunking merges heading-only chunks (<50 chars) with the next chunk to improve RAG embedding quality

## v0.0.8

- **Wikipedia / MediaWiki onlyMainContent fix** — `onlyMainContent: true` now correctly extracts article text from Wikipedia pages (~49% size reduction). Previously the `<html>` element's `class="vector-toc-available"` matched the `"toc"` noise pattern via substring, removing the entire page
- **3-tier noise pattern matching** — noise class/id matching now uses substring (long patterns), exact-token (short/ambiguous: `toc`, `share`, `social`, `comment`, `related`), and prefix (`ad-`, `ads-`) matching to avoid false positives
- **Structural element guard** — noise handler never removes `<html>`, `<head>`, `<body>`, or `<main>` elements
- **Re-clean after readability** — readability output is re-cleaned to strip residual noise (infobox, navbox, catlinks) that survives inside broad containers
- **Wikipedia-aware readability** — added `.mw-parser-output`, `#mw-content-text`, `#bodyContent` to scored selectors; priority/scored selectors that wrap >90% of body are skipped
- **BYOK LLM extraction** — per-request `llmApiKey`, `llmProvider`, `llmModel` fields for bring-your-own-key structured extraction without server config
- **JSON format validation** — `formats: ["json"]` without `jsonSchema` now returns a 400 error instead of a warning
- **Block detection skip** — pages >50 KB skip interstitial/block detection (no more false "blocked by anti-bot" on Wikipedia)
- **Null byte URL rejection** — URLs with `%00` or null bytes rejected at validation
- **Request timeout** — default timeout bumped from 60s to 120s
- **Dockerfile fix** — corrected `cargo build` flags, added `config.docker.toml`

## v0.0.7

- **`success: false` on 4xx targets** — scraping a 403/404/429 target with minimal body now correctly returns `success: false` with error details, instead of `success: true` with a warning. Targets with real content (custom error pages) still return `success: true` with a warning
- **JS renderer fallback warning** — when `renderJs: true` is requested but no CDP renderer is available, the response now includes `rendered_with: "http_only_fallback"` and a warning instead of silently falling back
- **CDP health check** — `is_available()` now runs a real `Browser.getVersion` command instead of just testing the WebSocket connection
- **Specific error messages** — unknown formats now return descriptive errors (e.g., `"Unknown format 'extract'. Valid formats: ..."`) instead of generic 422
- **`"extract"` format alias** — `formats: ["extract"]` and `formats: ["llm-extract"]` are now accepted as aliases for `"json"` (Firecrawl compatibility)
- **Chunk dedup by default** — deduplication is now enabled by default for all chunking strategies; separator-only chunks (`---`, `***`) are filtered out
- **Chunk relevance scores** — chunks now return `{ content, score, index }` objects instead of plain strings when a query is provided
- **Map timeout** — `/v1/map` accepts a `timeout` parameter (default 120s, max 300s) to prevent 502s on large sites
- **Stealth + JS rendering fix** — `stealth: true` with `renderJs: true` no longer bypasses CDP; the shared renderer is used with stealth headers injected
- **BM25 NaN guard** — prevents `NaN` scores when all chunks are empty

## v0.0.6

- **Crate READMEs on crates.io** — all 7 crates now have detailed README documentation visible on their crates.io pages, with usage examples, API docs, and installation instructions

## v0.0.5

- **`crw-cli` now on crates.io** — install the standalone CLI with `cargo install crw-cli` and scrape URLs without running a server
- **Parallelized release workflow** — crate publishing uses tiered parallelism, cutting release time by ~2.25 minutes
- **CLI and MCP install docs** — README now includes `cargo install` instructions for both `crw-cli` and `crw-mcp`

## v0.0.4

- **Hardened rendering and warning semantics** — improved reliability of the rendering pipeline and warning detection logic
- **XPath output escaping** — XPath extraction results are now properly escaped to prevent injection
- **Broadened status warnings** — expanded HTTP status code range that triggers warning metadata
- **Capped interstitial scan** — bounded interstitial page detection to avoid excessive scanning
- **Clippy cleanup** — simplified status code checks for cleaner, idiomatic Rust

## v0.0.3

- **Warning-aware target handling** — 4xx and anti-bot targets now return `success: true` with `warning` and `metadata.statusCode`
- **More reliable JS rendering** — CDP navigation now waits for real page lifecycle completion before applying `waitFor`
- **Stealth decompression fix** — gzip and brotli responses decode cleanly instead of leaking garbled binary payloads
- **Crawl compatibility** — `limit`, `maxPages`, and `max_pages` now normalize to the same crawl cap
- **XPath and chunking fixes** — XPath returns all matches, chunk overlap/dedupe is supported, and scorer rank order is preserved

## v0.0.2

- **CSS selector & XPath** — target specific DOM elements before Markdown conversion (`cssSelector`, `xpath`)
- **Chunking strategies** — split content into topic, sentence, or regex-delimited chunks for RAG pipelines (`chunkStrategy`)
- **BM25 & cosine filtering** — rank chunks by relevance to a query and return top-K results (`filterMode`, `topK`)
- **Better Markdown** — switched to `htmd` (Turndown.js port): tables, code block languages, nested lists all render correctly
- **Stealth mode** — rotate User-Agent from a built-in Chrome/Firefox/Safari pool and inject 12 browser-like headers (`stealth: true`)
- **Per-request proxy** — override the global proxy on a per-request basis (`proxy: "http://..."`)
- **Rate limit jitter** — randomized delay between requests to avoid uniform traffic fingerprinting
- **`crw-server setup`** — one-command JS rendering setup: downloads LightPanda, creates `config.local.toml`

## v0.0.1

- **Firecrawl-compatible REST API** — `/v1/scrape`, `/v1/crawl`, `/v1/map` with identical request/response format
- **6 output formats** — markdown, HTML, cleaned HTML, raw HTML, plain text, links, structured JSON
- **LLM structured extraction** — JSON schema in, validated structured data out (Anthropic tool_use + OpenAI function calling)
- **JS rendering** — auto-detect SPAs via heuristics, render via LightPanda, Playwright, or Chrome (CDP)
- **BFS crawler** — async crawl with rate limiting, robots.txt, sitemap support, concurrent jobs
- **MCP server** — built-in stdio + HTTP transport for Claude Code and Claude Desktop
- **SSRF protection** — private IPs, cloud metadata, IPv6, dangerous URI filtering
- **Docker ready** — multi-stage build with LightPanda sidecar
