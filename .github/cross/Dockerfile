ARG CROSS_BASE_IMAGE
FROM --platform=linux/amd64 ${CROSS_BASE_IMAGE}

ARG ZIG_VERSION=0.15.2

RUN apt-get update \
    && apt-get install --assume-yes --no-install-recommends ca-certificates curl xz-utils \
    && curl --fail --location --silent --show-error \
        "https://ziglang.org/download/${ZIG_VERSION}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz" \
        | tar --extract --xz --directory /opt \
    && ln --symbolic "/opt/zig-x86_64-linux-${ZIG_VERSION}/zig" /usr/local/bin/zig \
    && rm --recursive --force /var/lib/apt/lists/*
