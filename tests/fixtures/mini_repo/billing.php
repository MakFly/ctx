<?php

use App\Services\PaymentGateway;

final class BillingController
{
    public function charge(string $paymentId): bool
    {
        return PaymentGateway::capture($paymentId);
    }
}

Route::post('/payments', [BillingController::class, 'charge']);
