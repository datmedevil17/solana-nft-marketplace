#![allow(unexpected_cfgs)]
#![allow(deprecated)]
use anchor_lang::prelude::*;
use anchor_spl::token::{Token, TokenAccount};

declare_id!("MKTpLcXGQHkihHzwwxNX6Zw4YXtWWPzkjHRBEtcSWh3");

#[program]
pub mod marketplace {
    use super::*;

    /// Initialize the marketplace with admin authority
    pub fn initialize_marketplace(
        ctx: Context<InitializeMarketplace>,
        fee_basis_points: u16,
        treasury_bump: u8,
    ) -> Result<()> {
        require!(fee_basis_points <= 1000, MarketplaceError::FeeTooHigh); // Max 10%
        
        let marketplace = &mut ctx.accounts.marketplace;
        marketplace.authority = ctx.accounts.authority.key();
        marketplace.fee_basis_points = fee_basis_points;
        marketplace.treasury = ctx.accounts.treasury.key();
        marketplace.treasury_bump = treasury_bump;
        marketplace.is_paused = false;
        marketplace.total_volume = 0;
        marketplace.total_sales = 0;
        marketplace.bump = ctx.bumps.marketplace;

        emit!(MarketplaceInitialized {
            authority: marketplace.authority,
            fee_basis_points,
            treasury: marketplace.treasury,
        });

        Ok(())
    }

    /// Update marketplace fee (only admin)
    pub fn update_fee(ctx: Context<UpdateFee>, new_fee_basis_points: u16) -> Result<()> {
        require!(new_fee_basis_points <= 1000, MarketplaceError::FeeTooHigh);
        
        let marketplace = &mut ctx.accounts.marketplace;
        let old_fee = marketplace.fee_basis_points;
        marketplace.fee_basis_points = new_fee_basis_points;

        emit!(FeeUpdated {
            old_fee,
            new_fee: new_fee_basis_points,
            authority: ctx.accounts.authority.key(),
        });

        Ok(())
    }

    /// Update marketplace authority (only current admin)
    pub fn update_authority(ctx: Context<UpdateAuthority>, new_authority: Pubkey) -> Result<()> {
        let marketplace = &mut ctx.accounts.marketplace;
        let old_authority = marketplace.authority;
        marketplace.authority = new_authority;

        emit!(AuthorityUpdated {
            old_authority,
            new_authority,
        });

        Ok(())
    }

    /// Withdraw accumulated fees (only admin)
    pub fn withdraw_fees(ctx: Context<WithdrawFees>, amount: u64) -> Result<()> {
        let marketplace = &ctx.accounts.marketplace;
        let treasury = &mut ctx.accounts.treasury;
        let authority = &ctx.accounts.authority;

        require!(treasury.lamports() >= amount, MarketplaceError::InsufficientFunds);

        // Transfer SOL from treasury to authority
        **treasury.lamports.borrow_mut() -= amount;
        **authority.lamports.borrow_mut() += amount;

        emit!(FeesWithdrawn {
            amount,
            authority: authority.key(),
        });

        Ok(())
    }

    /// Pause/unpause marketplace (only admin)
    pub fn pause_marketplace(ctx: Context<PauseMarketplace>, pause: bool) -> Result<()> {
        let marketplace = &mut ctx.accounts.marketplace;
        marketplace.is_paused = pause;

        emit!(MarketplacePaused {
            is_paused: pause,
            authority: ctx.accounts.authority.key(),
        });

        Ok(())
    }

    /// Update marketplace stats (internal use by other contracts)
    pub fn update_stats(ctx: Context<UpdateStats>, sale_amount: u64) -> Result<()> {
        let marketplace = &mut ctx.accounts.marketplace;
        marketplace.total_volume = marketplace.total_volume.checked_add(sale_amount)
            .ok_or(MarketplaceError::MathOverflow)?;
        marketplace.total_sales = marketplace.total_sales.checked_add(1)
            .ok_or(MarketplaceError::MathOverflow)?;

        Ok(())
    }

    /// Calculate platform fee for a given sale amount
    pub fn calculate_fee(ctx: Context<CalculateFee>, sale_amount: u64) -> Result<u64> {
        let marketplace = &ctx.accounts.marketplace;
        let fee = sale_amount
            .checked_mul(marketplace.fee_basis_points as u64)
            .ok_or(MarketplaceError::MathOverflow)?
            .checked_div(10000)
            .ok_or(MarketplaceError::MathOverflow)?;
        
        Ok(fee)
    }
}

#[derive(Accounts)]
pub struct InitializeMarketplace<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + MarketplaceState::INIT_SPACE,
        seeds = [b"marketplace"],
        bump
    )]
    pub marketplace: Account<'info, MarketplaceState>,
    
    #[account(mut)]
    pub authority: Signer<'info>,
    
    /// CHECK: Treasury account for collecting fees
    #[account(
        mut,
        seeds = [b"treasury"],
        bump
    )]
    pub treasury: AccountInfo<'info>,
    
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateFee<'info> {
    #[account(
        mut,
        seeds = [b"marketplace"],
        bump = marketplace.bump,
        has_one = authority
    )]
    pub marketplace: Account<'info, MarketplaceState>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct UpdateAuthority<'info> {
    #[account(
        mut,
        seeds = [b"marketplace"],
        bump = marketplace.bump,
        has_one = authority
    )]
    pub marketplace: Account<'info, MarketplaceState>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct WithdrawFees<'info> {
    #[account(
        seeds = [b"marketplace"],
        bump = marketplace.bump,
        has_one = authority,
        has_one = treasury
    )]
    pub marketplace: Account<'info, MarketplaceState>,
    
    #[account(mut)]
    pub authority: Signer<'info>,
    
    /// CHECK: Treasury account validated by marketplace state
    #[account(
        mut,
        seeds = [b"treasury"],
        bump = marketplace.treasury_bump
    )]
    pub treasury: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct PauseMarketplace<'info> {
    #[account(
        mut,
        seeds = [b"marketplace"],
        bump = marketplace.bump,
        has_one = authority
    )]
    pub marketplace: Account<'info, MarketplaceState>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct UpdateStats<'info> {
    #[account(
        mut,
        seeds = [b"marketplace"],
        bump = marketplace.bump
    )]
    pub marketplace: Account<'info, MarketplaceState>,
}

#[derive(Accounts)]
pub struct CalculateFee<'info> {
    #[account(
        seeds = [b"marketplace"],
        bump = marketplace.bump
    )]
    pub marketplace: Account<'info, MarketplaceState>,
}

#[account]
#[derive(InitSpace)]
pub struct MarketplaceState {
    pub authority: Pubkey,           // 32
    pub fee_basis_points: u16,       // 2 (e.g., 250 = 2.5%)
    pub treasury: Pubkey,            // 32
    pub treasury_bump: u8,           // 1
    pub is_paused: bool,             // 1
    pub total_volume: u64,           // 8
    pub total_sales: u64,            // 8
    pub bump: u8,                    // 1
}

#[event]
pub struct MarketplaceInitialized {
    pub authority: Pubkey,
    pub fee_basis_points: u16,
    pub treasury: Pubkey,
}

#[event]
pub struct FeeUpdated {
    pub old_fee: u16,
    pub new_fee: u16,
    pub authority: Pubkey,
}

#[event]
pub struct AuthorityUpdated {
    pub old_authority: Pubkey,
    pub new_authority: Pubkey,
}

#[event]
pub struct FeesWithdrawn {
    pub amount: u64,
    pub authority: Pubkey,
}

#[event]
pub struct MarketplacePaused {
    pub is_paused: bool,
    pub authority: Pubkey,
}

#[error_code]
pub enum MarketplaceError {
    #[msg("Fee basis points cannot exceed 1000 (10%)")]
    FeeTooHigh,
    #[msg("Insufficient funds in treasury")]
    InsufficientFunds,
    #[msg("Marketplace is currently paused")]
    MarketplacePaused,
    #[msg("Math overflow occurred")]
    MathOverflow,
    #[msg("Unauthorized access")]
    Unauthorized,
}

// Helper functions for other contracts to use
impl MarketplaceState {
    pub const INIT_SPACE: usize = 32 + 2 + 32 + 1 + 1 + 8 + 8 + 1; // 85 bytes
    
    pub fn is_paused(&self) -> bool {
        self.is_paused
    }
    
    pub fn get_fee_basis_points(&self) -> u16 {
        self.fee_basis_points
    }
    
    pub fn get_treasury(&self) -> Pubkey {
        self.treasury
    }
    
    pub fn calculate_platform_fee(&self, sale_amount: u64) -> Result<u64> {
        sale_amount
            .checked_mul(self.fee_basis_points as u64)
            .ok_or(MarketplaceError::MathOverflow)?
            .checked_div(10000)
            .ok_or(MarketplaceError::MathOverflow)
            .map_err(|e| e.into())
    }
}

// Cross-program invocation helper for other contracts
pub fn check_marketplace_active(marketplace: &Account<MarketplaceState>) -> Result<()> {
    require!(!marketplace.is_paused, MarketplaceError::MarketplacePaused);
    Ok(())
}