ifneq ($(KERNELRELEASE),)
obj-m := gitfs.o
gitfs-y := vfs.o refs.o http.o dns.o smart.o cache.o codec.o
ccflags-y := -Werror

quiet_cmd_gitfs_codec = CARGO   $@
      cmd_gitfs_codec = $(CONFIG_SHELL) $(src)/build-codec.sh "$(src)" "$(obj)" "$(srctree)" "$(LD)" "$(OBJCOPY)"
gitfs_core_sources := $(shell rg --files "$(src)/../../src" "$(src)/../../crates/git-zero-core" \
                       "$(src)/../../crates/git-zero-client" -g '*.rs' -g Cargo.toml)

$(obj)/codec.o: $(wildcard $(src)/codec/src/*.rs) $(src)/codec/Cargo.toml $(src)/build-codec.sh \
               $(src)/../../Cargo.toml $(src)/../../Cargo.lock $(gitfs_core_sources) FORCE
	$(call if_changed,gitfs_codec)
targets += codec.o
else
KDIR ?= /lib/modules/$(shell uname -r)/build
.DEFAULT_GOAL := all
.PHONY: all clean test
all:
	$(MAKE) -C $(KDIR) M=$(CURDIR) modules
clean:
	$(MAKE) -C $(KDIR) M=$(CURDIR) clean
	rm -rf build
test:
	cargo test -p git-zero-kernel-codec
endif
