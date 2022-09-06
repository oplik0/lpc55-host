window.SIDEBAR_ITEMS = {"constant":[["CUSTOMER_SETTINGS_SCRATCH_ADDRESS",""],["FACTORY_SETTINGS_ADDRESS",""]],"enum":[["BootSpeed","Purposely swapped 48MHz and 96MHz values from what they are in reference manual (Rev. 2.1).  These are the correct values."],["IspMode",""],["RotKeyStatus",""],["TrustzoneMode",""]],"mod":[["debug",""]],"struct":[["ActivationCode",""],["BootConfiguration",""],["CustomerSettings","See `CustomerSettingsArea` documentation for how this part of the configuration is used and updated by the ROM bootloader."],["CustomerSettingsArea","This is a bit of an interesting construction."],["FactorySettings",""],["FactorySettingsProgInProgress","CMPA Page programming on going. This field shall be set to 0x5CC55AA5 in the active CFPA page each time CMPA page programming is going on. It shall always be set to 0x00000000 in the CFPA scratch area."],["Header",""],["Keycode","Input to regenerate keys “stored” in the PUF."],["Keystore","All the keys :)"],["KeystoreHeader",""],["MonotonicCounter",""],["NxpArea","This is incorrect. There’s more in the NMPA spreadsheet section!"],["PrinceConfiguration",""],["PrinceIvCode","Generated and used only by bootloader."],["PrinceSubregion",""],["ProtectedFlash","For a graphical overview: https://whimsical.com/lpc55-flash-memory-map-4eU3ei4wsqiAD7D2cAiv5s"],["RawCustomerData",""],["RawVendorUsage",""],["RotKeysStatus",""],["SecureBootConfiguration",""],["UsbId","The PID/VID pair of the bootloader can be set arbitrarily, the default of `1fc9:0021` is used if the values zero are not changed."],["WrappedCustomerSettings","Type enabling `lpc55 configure customer-settings` to share config file with the secure/signed firmware generation commands. Serializes `CustomerSettings` with a `[customer-settings]` header."],["WrappedFactorySettings","Type enabling `lpc55 configure factory-settings` to share config file with the secure/signed firmware generation commands. Serializes `FactorySettings` with a `[factory-settings]` header."]],"trait":[["BigArray",""],["CustomerSettingsCustomerData",""],["CustomerSettingsVendorUsage",""],["FactorySettingsCustomerData",""],["FactorySettingsVendorUsage",""]]};