# Kiku's model host as an image: kiku-engine on NVIDIA's CUDA 12 + cuDNN 9 runtime, so a GPU host needs only
# the NVIDIA driver and container toolkit. Built from the release tarball (scripts/pack-linux.sh), put in the
# build context with this file's -f:
#   docker build -t kiku-engine -f Dockerfile <folder holding kiku-engine-<version>-linux-x64.tar.gz>
#   docker run --rm -v kiku:/data kiku-engine --download parakeet-unified --device gpu
#   docker run --gpus all -p 7300:7300 -v kiku:/data kiku-engine
FROM nvidia/cuda:12.9.1-cudnn-runtime-ubuntu22.04

# ADD unpacks a local tarball: /opt/kiku-engine/kiku-engine and its libraries.
ADD kiku-engine-*-linux-x64.tar.gz /opt/
RUN useradd --system --uid 10001 --home-dir /data --no-create-home kiku \
    && mkdir -p /data && chown kiku:kiku /data

ENV KIKU_MODELS_DIR=/data/models
VOLUME /data
USER kiku
EXPOSE 7300
ENTRYPOINT ["/opt/kiku-engine/kiku-engine"]
CMD ["--serve", "--listen", "0.0.0.0:7300", "--state", "/data/host"]
