FROM node:24-alpine
WORKDIR /app
COPY server.mjs verify-manifest.mjs release-key.pub index.html install.sh install.ps1 ./
COPY public ./public
# The deploy uploads no archives; each one redirects to its GitHub release.
RUN node verify-manifest.mjs --allow-missing-archives
ENV NODE_ENV=production
ENV BAKE_ARCHIVE_REDIRECT=https://github.com/Justar96/bake/releases/download
EXPOSE 8080
CMD ["node", "server.mjs"]
