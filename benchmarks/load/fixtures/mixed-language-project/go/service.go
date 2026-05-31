package service

func BuildServiceName(prefix string) string {
	return prefix + "-fixture-service"
}

func StartService(prefix string) string {
	return BuildServiceName(prefix)
}
