.PHONE = deploy-unit-file active-unit-file

deploy-unit-file:
	scp telewind.service mvps:/etc/systemd/system/telewind.service

active-unit-file:
	ssh mvps systemctl enable telewind