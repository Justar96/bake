FROM node:24-alpine
WORKDIR /app
COPY server.mjs verify-manifest.mjs release-key.pub index.html install.sh install.ps1 ./
COPY public ./public
RUN node verify-manifest.mjs
ENV NODE_ENV=production
EXPOSE 8080
CMD ["node", "server.mjs"]
