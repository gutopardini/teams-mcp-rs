#!/usr/bin/env python3
import json
import sys
from urllib.parse import parse_qs, quote, urlparse

if sys.argv[1:] != ["--auth-webview-helper"]:
    raise SystemExit(2)

request = json.load(sys.stdin)
params = parse_qs(urlparse(request["authorize_url"]).query)
if params.get("code_challenge_method") != ["S256"]:
    raise SystemExit(3)
if len(params.get("scope", [])) != 1:
    raise SystemExit(5)
if "https://graph.microsoft.com/Chat.Read" not in params.get("scope", [""])[0]:
    raise SystemExit(4)

state = params["state"][0]
callback = request["redirect_uri"] + "?code=fictional-code&state=" + quote(state)
json.dump({"status": "callback", "url": callback}, sys.stdout)
