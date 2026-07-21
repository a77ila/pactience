FROM archlinux:base-devel

ENV RUSTUP_HOME=/usr/local/rustup
ENV CARGO_HOME=/usr/local/cargo
ENV PATH=/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin

RUN pacman -Syu --noconfirm \
    ca-certificates \
    curl \
    git \
    bash \
    sudo \
    nodejs \
    npm \
    && pacman -Scc --noconfirm

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain 1.96.0 --profile minimal \
    && rustup component add rustfmt clippy

RUN useradd -m -G wheel builder \
    && echo '%wheel ALL=(ALL:ALL) NOPASSWD: ALL' > /etc/sudoers.d/wheel-nopasswd \
    && chmod 0440 /etc/sudoers.d/wheel-nopasswd \
    && chown -R builder:builder /usr/local/cargo /usr/local/rustup

USER builder
WORKDIR /tmp

RUN git clone https://aur.archlinux.org/paru.git \
    && cd paru \
    && makepkg --syncdeps --install --needed --noconfirm \
    && cd /tmp \
    && rm -rf paru

USER root

RUN chown -R root:root /usr/local/cargo /usr/local/rustup \
    && npm install -g @moonshot-ai/kimi-code@latest

WORKDIR /workspace

CMD ["kimi"]
