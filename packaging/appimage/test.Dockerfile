ARG BASE=ubuntu:22.04@sha256:5ec03bb3441e8b0bf3b4f9cd4629a1ae763010dc3035bb8da3ae6cf026486401
FROM ${BASE}
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    xvfb xauth x11-utils dbus-x11 bubblewrap xdg-dbus-proxy ca-certificates \
    fonts-dejavu-core fontconfig-config gnupg gpgsm at-spi2-core openbox \
    fuse3 python3 python3-gi gir1.2-atspi-2.0 libxtst6 libglib2.0-bin \
    && rm -rf /var/lib/apt/lists/*
RUN if ! getent passwd 1000 >/dev/null; then useradd --uid 1000 --create-home probe; fi
