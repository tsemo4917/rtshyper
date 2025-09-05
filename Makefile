# DISK is the path to the ROOTFS for VM0 when `make run` on QEMU
# DISK can be set on the command line
DISK :=

# Compile
ARCH ?= aarch64
PROFILE ?= release
BOARD ?= tx2
# features, seperate with comma `,`
FEATURES =
export TEXT_START ?= 0x83000000

# CROSS_COMPILE can be set on the command line
export CROSS_COMPILE ?= aarch64-none-elf-
export CFLAGS := -ffreestanding -mgeneral-regs-only

# export CC for C-library cross compiling
ifneq ($(LLVM),)
unexport CROSS_COMPILE
export CC	= clang
LD			= ld.lld
AR			= llvm-ar
NM			= llvm-nm
READEL		= llvm-readelf
STRIP		= llvm-strip
OBJDUMP		= llvm-objdump
OBJCOPY		= llvm-objcopy
GDB			= lldb
else
export CC	= $(CROSS_COMPILE)gcc
LD			= $(CROSS_COMPILE)ld
AR			= $(CROSS_COMPILE)ar
NM			= $(CROSS_COMPILE)nm
READELF		= $(CROSS_COMPILE)readelf
STRIP		= $(CROSS_COMPILE)strip
OBJDUMP		= $(CROSS_COMPILE)objdump
OBJCOPY		= $(CROSS_COMPILE)objcopy
GDB			= ${CROSS_COMPILE}gdb
endif
QEMU = qemu-system-$(ARCH)

IMAGE=rtshyper_rs

TARGET_DIR=target/${ARCH}/${PROFILE}

TFTP_SERVER ?= root@192.168.106.153:/tftp

UBOOT_IMAGE ?= RTShyperImage

CARGO_ACTION ?= build

# Cargo flags.
CARGO_FLAGS ?= -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem --target ${ARCH}.json --no-default-features --features "${BOARD},${FEATURES}"
ifeq (${PROFILE}, release)
CARGO_FLAGS += --release
else ifneq (${PROFILE}, debug)
$(error bad PROFILE: ${PROFILE})
endif

.PHONY: build cargo clippy upload qemu tx2 pi4 run debug clean gdb

cargo:
	cargo fmt
	cargo ${CARGO_ACTION} ${CARGO_FLAGS}

clippy:
	$(MAKE) cargo CARGO_ACTION=clippy

build: cargo
	${OBJDUMP} --demangle -d ${TARGET_DIR}/${IMAGE} > ${TARGET_DIR}/t.asm
	${OBJCOPY} ${TARGET_DIR}/${IMAGE} -O binary ${TARGET_DIR}/${IMAGE}.bin

upload: build
	@mkimage -n ${IMAGE} -A arm64 -O linux -T kernel -C none -a $(TEXT_START) -e $(TEXT_START) -d ${TARGET_DIR}/${IMAGE}.bin ${TARGET_DIR}/${UBOOT_IMAGE}
	@echo "*** Upload Image ${UBOOT_IMAGE} ***"
	@scp ${TARGET_DIR}/${UBOOT_IMAGE} ${TFTP_SERVER}/${UBOOT_IMAGE}
	@echo "tftp 0x8a000000 \$${serverip}:${UBOOT_IMAGE}; bootm start 0x8a000000 - 0x80000000; bootm loados; bootm go"

qemu:
	$(MAKE) build BOARD=qemu TEXT_START=0x40080000

tx2:
	$(MAKE) upload BOARD=tx2 TEXT_START=0x83000000

pi4:
	$(MAKE) upload BOARD=pi4 TEXT_START=0xF0080000
	scp ./image/pi4_fin.dtb ${TFTP_SERVER}/pi4_dtb

ifeq ($(ARCH),aarch64)
QEMU_OPTIONS = -machine virt,virtualization=on,gic-version=2 -m 8g -cpu cortex-a57 -smp 4
else
$(error bad arch: $(ARCH))
endif

QEMU_OPTIONS += -display none -global virtio-mmio.force-legacy=false -kernel ${TARGET_DIR}/${IMAGE}.bin

QEMU_SERIAL_OPTIONS = -serial mon:stdio #\
	-serial telnet:localhost:12345,server
QEMU_OPTIONS += $(QEMU_SERIAL_OPTIONS)

QEMU_NETWORK_OPTIONS = -netdev user,id=n0,hostfwd=tcp::5555-:22 -device virtio-net-device,bus=virtio-mmio-bus.24,netdev=n0
QEMU_OPTIONS += $(QEMU_NETWORK_OPTIONS)

ifneq ($(DISK),)
QEMU_DISK_OPTIONS = -drive file=${DISK},if=none,id=x0 -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.25
QEMU_OPTIONS += $(QEMU_DISK_OPTIONS)
else
$(warning DISK is not offered)
endif

run: qemu
	${QEMU} ${QEMU_OPTIONS}

debug: QEMU_OPTIONS += -s -S
debug: run

gdb:
	${GDB} -x gdb/${ARCH}.gdb

clean:
	cargo clean
