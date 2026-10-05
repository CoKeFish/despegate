# The landing page: static files served by nginx. Built by Coolify from this
# directory; nothing in it is secret.
FROM nginx:1.27-alpine
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY index.html logo.svg favicon.ico /usr/share/nginx/html/
COPY fonts /usr/share/nginx/html/fonts
EXPOSE 80
HEALTHCHECK --interval=30s --timeout=3s CMD wget -qO- http://127.0.0.1/ >/dev/null || exit 1
