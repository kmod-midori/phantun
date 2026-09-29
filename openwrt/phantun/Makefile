include $(TOPDIR)/rules.mk

PKG_NAME:=phantun
PKG_VERSION:=$(shell cat $(CURDIR)/prebuilt/version)
PKG_RELEASE:=1
PKG_LICENSE:=MIT Apache-2.0
PKG_LICENSE_FILES:=LICENSE-MIT LICENSE-APACHE

include $(INCLUDE_DIR)/package.mk
# These binaries include musl and libgcc; do not inject the SDK libc dependency.
PKG_DEFAULT_DEPENDS:=

define Package/phantun
  SECTION:=net
  CATEGORY:=Network
  TITLE:=UDP to TCP obfuscator
  URL:=https://github.com/dndx/phantun
  DEPENDS:=+kmod-tun
endef

define Package/phantun/description
  Phantun client and server with a multi-instance UCI/procd service.
endef

# Package the already-built static musl release binaries without recompiling.
define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
	$(CP) ./prebuilt/. $(PKG_BUILD_DIR)/
endef

define Build/Configure
endef

define Build/Compile
endef

define Package/phantun/conffiles
/etc/config/phantun
endef

define Package/phantun/install
	$(INSTALL_DIR) $(1)/usr/bin $(1)/etc/init.d $(1)/etc/config
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/phantun_client $(1)/usr/bin/phantun_client
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/phantun_server $(1)/usr/bin/phantun_server
	$(INSTALL_BIN) ./files/phantun.init $(1)/etc/init.d/phantun
	$(INSTALL_CONF) ./files/phantun.config $(1)/etc/config/phantun
endef

$(eval $(call BuildPackage,phantun))
