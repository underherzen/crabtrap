//! Готовые фрагменты кода (Snippets в Script Editor, как Built-in Snippet
//! Code в Proxyman). Вставляются в место курсора внутри `onRequest` /
//! `onResponse` — пишут в переменные `request` / `response`.

/// Фрагмент: подпись в меню и код.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snippet {
    pub title: &'static str,
    pub code: &'static str,
}

pub const SNIPPETS: &[Snippet] = &[
    Snippet {
        title: "Request: add or change a header",
        code: "request.headers[\"X-Debug\"] = \"1\";\n",
    },
    Snippet {
        title: "Request: remove a header",
        code: "delete request.headers[\"Authorization\"];\n",
    },
    Snippet {
        title: "Request: change a query parameter",
        code: "request.queries[\"page\"] = \"2\";\n",
    },
    Snippet {
        title: "Request: send to another host",
        code: "request.host = \"staging.example.com\";\n",
    },
    Snippet {
        title: "Request: change the method",
        code: "request.method = \"POST\";\n",
    },
    Snippet {
        title: "Request: JSON body",
        code: "request.headers[\"Content-Type\"] = \"application/json\";\nrequest.body = { \"key\": \"value\" };\n",
    },
    Snippet {
        title: "Request: Bearer token from $sharedState",
        code: "if ($sharedState.token) {\n  request.headers[\"Authorization\"] = \"Bearer \" + $sharedState.token;\n}\n",
    },
    Snippet {
        title: "Response: change the status code",
        code: "response.statusCode = 500;\n",
    },
    Snippet {
        title: "Response: change a JSON field",
        code: "response.body[\"key\"] = \"new value\";\n",
    },
    Snippet {
        title: "Response: replace with JSON",
        code: "response.statusCode = 200;\nresponse.headers[\"Content-Type\"] = \"application/json\";\nresponse.body = { \"ok\": true };\n",
    },
    Snippet {
        title: "Response: allow CORS",
        code: "response.headers[\"Access-Control-Allow-Origin\"] = \"*\";\nresponse.headers[\"Access-Control-Allow-Headers\"] = \"*\";\nresponse.headers[\"Access-Control-Allow-Methods\"] = \"GET, POST, PUT, PATCH, DELETE, OPTIONS\";\n",
    },
    Snippet {
        title: "Response: save a token to $sharedState",
        code: "$sharedState.token = response.body[\"token\"];\n",
    },
    Snippet {
        title: "Response: replace text",
        code: "response.body = String(response.body).replaceAll(\"old\", \"new\");\n",
    },
    Snippet {
        title: "Log the request",
        code: "console.log(request.method, url, request.headers);\n",
    },
    Snippet {
        title: "Addons: base64, SHA-256, UUID",
        code: "const encoded = btoa(\"text\");\nconst hash = $crypto.sha256(\"text\");\nconst id = $crypto.randomUUID();\n",
    },
];
