# API Playground

The Playground is the fastest way to experiment with CRW without wiring up your own client first. Use it when you want to validate a URL, compare options, or inspect response shapes before writing code.

## Open The Playground

[Open fastcrw Playground](https://fastcrw.com/playground)

## Best Uses

- validate that a target URL works before you automate it,
- compare `markdown` vs `html` vs `json`,
- test selectors or JS rendering settings,
- try a walled page in auto or max mode before you pay for it in code,
- and confirm response fields before you wire the request into your app or agent.

## Standard, auto and max

The scrape toolbar has a Standard / Auto / Max switch next to the format. Auto and max run the page through the [auto and max mode](/scraping#auto-and-max-mode) of `/v1/scrape`, billed to your account the same way an API call is, and show a cap field for `maxCredits`. The badge above the URL shows what the selected mode costs, and the code panel adds `mode` and `maxCredits` to the snippet. They need a signed-in account on the Standard plan or higher.

## Recommended Workflow

1. Start with `scrape`
2. Request only `markdown`
3. Confirm the output looks right
4. Add selectors, JS rendering, or extraction only if needed

That sequence keeps debugging simple and makes the endpoint examples almost copy-paste.

## When Not To Use It

Skip the Playground if you already know the request shape you need and just want production code. In that case, start with [Quick Start](#quick-start) or the specific endpoint page under [REST API](#rest-api).

## What To Read Next

- [Quick Start](#quick-start)
- [Scrape](#scraping)
- [Response Shapes](#response-shapes)
