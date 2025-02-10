FROM ubuntu@sha256:7a57c69fe1e9d5b97c5fe649849e79f2cfc3bf11d10bbd5218b4eb61716aebe6

RUN apt-get update -y \
    && apt-get install -y --no-install-recommends \
    # LLVM binaries need this
    libncurses5 \
    strace \
    lldb \
    && rm -rf /var/lib/apt/lists/*