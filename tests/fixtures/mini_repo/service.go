package shop

import "context"

type CheckoutService struct{}

func (service *CheckoutService) Charge(ctx context.Context, amount int) error {
	return savePayment(amount)
}

func savePayment(amount int) error {
	return nil
}
