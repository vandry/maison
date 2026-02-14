# Kim's home automation custom logic

You probably do not want this.

Expose a web server:

```
server {
    # Must contain index.html and bundle.js
    root /where/the/built/htdocs/can/be/found;
    location /maison.Maison/ {
        proxy_pass http://maison/maison.Maison/;
        proxy_http_version 1.1;
        proxy_read_timeout 1800s;
    }
}

upstream maison {
    server unix:/tmp/maison.sock;
}
```
