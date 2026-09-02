"""Tiny WSGI app that does the same interpreter churn per request."""
import workload


def app(environ, start_response):
    fn = workload.make_code(30)
    for _ in range(5):
        fn(200)
    start_response("200 OK", [("content-type", "text/plain")])
    return [b"ok\n"]
